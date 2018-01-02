// Copyright (c) 2017 King's College London
// created by the Software Development Team <http://soft-dev.org/>
//
// The Universal Permissive License (UPL), Version 1.0
//
// Subject to the condition set forth below, permission is hereby granted to any
// person obtaining a copy of this software, associated documentation and/or
// data (collectively the "Software"), free of charge and under any and all
// copyright rights in the Software, and any and all patent rights owned or
// freely licensable by each licensor hereunder covering either (i) the
// unmodified Software as contributed to or provided by such licensor, or (ii)
// the Larger Works (as defined below), to deal in both
//
// (a) the Software, and
// (b) any piece of software and/or hardware listed in the lrgrwrks.txt file
// if one is included with the Software (each a "Larger Work" to which the Software
// is contributed by such licensors),
//
// without restriction, including without limitation the rights to copy, create
// derivative works of, display, perform, and distribute the Software and make,
// use, sell, offer for sale, import, export, have made, and have sold the
// Software and the Larger Work(s), and to sublicense the foregoing rights on
// either these or other terms.
//
// This license is subject to the following condition: The above copyright
// notice and either this complete permission notice or at a minimum a reference
// to the UPL must be included in all copies or substantial portions of the
// Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
// IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
// FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
// AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
// LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
// OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
// SOFTWARE.

use libc::{pid_t, c_char, c_void, getpid, size_t, c_int, geteuid};
use std::ffi::CString;
use std::path::PathBuf;
use errors::TraceMeError;
use std::fs::File;
use std::io::Read;
use Tracer;

// The sysfs path used to set perf permissions.
const PERF_PERMS_PATH: &'static str = "/proc/sys/kernel/perf_event_paranoid";

// FFI prototypes.
extern "C" {
    fn perf_pt_start_tracer(conf: *const PerfPTConf) -> *const c_void;
    fn perf_pt_stop_tracer(tr_ctx: *const c_void) -> c_int;
}

// Struct used to communicate a tracing configuration to the C code. Must
// stay in sync with the C code.
#[repr(C)]
struct PerfPTConf {
    target_pid: pid_t,
    trace_filename: *const c_char,
    map_filename: *const c_char,
    data_bufsize: size_t,
    aux_bufsize: size_t,
}

/// A tracer that uses the Linux Perf interface to Intel Processor Trace.
#[derive(Default)]
pub struct PerfPTTracer {
    /// Filename to store the trace to.
    trace_filename: String,
    /// PID to trace.
    target_pid: pid_t,
    /// Data buffer size, in pages. Must be a power of 2.
    data_bufsize: size_t,
    /// Aux buffer size, in pages. Must be a power of 2.
    aux_bufsize: size_t,
    /// Opaque C pointer representing the tracer context.
    tracer_ctx: Option<*const c_void>,
}

impl PerfPTTracer {
    /// Create a new tracer.
    ///
    /// Returns `Err` if the CPU doesn't support Intel Processor Trace.
    ///
    /// # Example
    ///
    /// ```
    /// use traceme::backends::PerfPTTracer;
    ///
    /// let res = PerfPTTracer::new();
    /// if res.is_ok() {
    ///     let tracer = res.unwrap().trace_filename("mytrace.ptt").data_bufsize(1024).target_pid(666);
    /// } else {
    ///     // CPU doesn't support Intel Processor Trace.
    /// }
    /// ```
    pub fn new() -> Result<Self, TraceMeError> {
        if Self::pt_supported() {
            return Ok(Self {
                trace_filename: String::from("traceme.ptt"),
                target_pid: unsafe { getpid() },
                tracer_ctx: None,
                data_bufsize: 64,
                aux_bufsize: 1024,
            });
        }
        Err(TraceMeError::HardwareSupport("Intel PT not supported by CPU".into()))
    }

    /// Set where to write the trace.
    ///
    /// # Arguments
    ///
    /// * `filename` - The filename in which to store trace packets.
    pub fn trace_filename(mut self, filename: &str) -> Self {
        self.trace_filename = String::from(filename);
        self
    }

    /// Select which PID to trace.
    ///
    /// By default, the current PID is traced.
    ///
    /// # Arguments
    ///
    /// * `pid` - The PID to trace.
    pub fn target_pid(mut self, pid: pid_t) -> Self {
        self.target_pid = pid;
        self
    }

    /// Set the PT data buffer size.
    ///
    /// # Arguments
    ///
    /// * `size` - The data buffer size to use.
    pub fn data_bufsize(mut self, size: usize) -> Self {
        self.data_bufsize = size;
        self
    }

    /// Set the PT aux buffer size.
    ///
    /// # Arguments
    ///
    /// * `size` - The aux buffer size to use.
    pub fn aux_bufsize(mut self, size: usize) -> Self {
        self.aux_bufsize = size;
        self
    }

    /// Make the map filename by setting/adding a ".map" extension to `trace_filename`.
    fn make_map_filename(trace_filename: &str) -> Result<String, TraceMeError> {
        let mut pb = PathBuf::from(trace_filename);
        if !pb.set_extension("map") {
            return Err(TraceMeError::InvalidFileName("".to_string()));
        }
        let map_filename = pb.to_str().ok_or(TraceMeError::InvalidFileName("".to_string()))?;
        Ok(String::from(map_filename))
    }

    fn check_perf_perms() -> Result<(), TraceMeError> {
        if unsafe { geteuid() } == 0 {
            // Root can always trace.
            return Ok(());
        }

        let mut f = File::open(&PERF_PERMS_PATH)?;
        let mut buf = String::new();
        f.read_to_string(&mut buf)?;
        let perm = buf.trim().parse::<i8>()?;
        if perm != -1 {
            let msg = format!("Tracing not permitted: you must be root or {} must contain -1",
                           PERF_PERMS_PATH);
            return Err(TraceMeError::TracingNotPermitted(msg));
        }

        Ok(())
    }

    /// Checks if the CPU supports Intel Processor Trace.
    fn pt_supported() -> bool {
        const LEAF: u32 = 0x07;
        const SUBPAGE: u32 = 0x0;
        const EBX_BIT: u32 = 1 << 25;
        let ebx_out: u32;

        unsafe {
            asm!(r"
                  mov $1, %eax;
                  mov $2, %ecx;
                  cpuid;"
                : "={ebx}" (ebx_out)
                : "i" (LEAF), "i" (SUBPAGE)
                : "eax", "ecx", "edx"
                : "volatile");
        }

        if ebx_out & (EBX_BIT) != 0 {
            return true;
        }
        false
    }
}

impl Tracer for PerfPTTracer {
    fn start_tracing(&mut self) -> Result<(), TraceMeError> {
        PerfPTTracer::check_perf_perms()?;
        if self.tracer_ctx.is_some() {
            return Err(TraceMeError::TracerAlreadyStarted);
        }
        if !self.trace_filename.ends_with(".ptt") {
            return Err(TraceMeError::InvalidFileName(String::from(self.trace_filename.clone())));
        }

        // Build the C configuration struct
        let map_filename_c = CString::new(PerfPTTracer::make_map_filename(&self.trace_filename)?)?;
        let trace_filename_c = CString::new(self.trace_filename.clone())?;
        let tr_conf = PerfPTConf {
            target_pid: self.target_pid,
            trace_filename: trace_filename_c.as_ptr(),
            map_filename: map_filename_c.as_ptr(),
            data_bufsize: self.data_bufsize,
            aux_bufsize: self.aux_bufsize,
        };

        // Call C
        let conf_ptr = &tr_conf as *const PerfPTConf;
        let opq_ptr = unsafe {
            perf_pt_start_tracer(conf_ptr)
        };
        if opq_ptr.is_null() {
            return Err(TraceMeError::CFailure);
        }
        self.tracer_ctx = Some(opq_ptr);
        Ok(())
    }

    fn stop_tracing(&mut self) -> Result<(), TraceMeError> {
        let tr_ctx = self.tracer_ctx.ok_or(TraceMeError::TracerNotStarted)?;
        let rc = unsafe {
            perf_pt_stop_tracer(tr_ctx)
        };
        if rc == -1 {
            return Err(TraceMeError::CFailure);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::PerfPTTracer;
    use ::test_helpers;

    fn run_test_helper<F>(f: F)  where F: Fn(PerfPTTracer) {
        let res = PerfPTTracer::new();
        // Only run the test if the CPU supports Intel PT.
        if res.is_ok() {
            f(res.unwrap());
        }
    }

    #[test]
    fn test_basic_usage() {
        run_test_helper(test_helpers::test_basic_usage);
    }

    #[test]
    fn test_already_started() {
        run_test_helper(test_helpers::test_already_started);
    }

    #[test]
    fn test_not_started() {
        run_test_helper(test_helpers::test_not_started);
    }

    #[test]
    fn test_map_filename() {
        assert!(PerfPTTracer::make_map_filename("trace.ptt").unwrap() == "trace.map");
    }
}