// A raw serial port, in termios, because that is what a serial port is.
//
// Non-blocking with an explicit deadline everywhere: a counter unplugged
// mid-read must not wedge the monitor, and poll() is the only thing standing
// between this program and a hang with no output.
use std::ffi::CString;
use std::io;
use std::time::{Duration, Instant};

pub struct Serial {
    fd: libc::c_int,
    pub timeout: Duration,
}

/// The port is open in another process, and is being left alone.
///
/// WHY A SERIAL PORT NEEDS A LOCK: two processes reading one tty do not each
/// get the stream, they get a share of it each, and neither is told. A logger
/// and a monitor running together would quietly halve both their counts --
/// a wrong reading that looks entirely plausible.
#[derive(Debug)]
pub enum OpenError {
    Busy,
    Io(io::Error),
}

impl std::fmt::Display for OpenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OpenError::Busy => write!(f, "the port is already open"),
            OpenError::Io(e) => write!(f, "{}", e),
        }
    }
}

fn baud_constant(baud: u32) -> Option<libc::speed_t> {
    Some(match baud {
        115200 => libc::B115200,
        57600 => libc::B57600,
        38400 => libc::B38400,
        19200 => libc::B19200,
        9600 => libc::B9600,
        _ => return None,
    })
}

impl Serial {
    pub fn open(path: &str, baud: u32, timeout: Duration) -> Result<Serial, OpenError> {
        let c = CString::new(path).map_err(|_| {
            OpenError::Io(io::Error::new(io::ErrorKind::InvalidInput, "path"))
        })?;
        let fd = unsafe {
            libc::open(c.as_ptr(), libc::O_RDWR | libc::O_NOCTTY | libc::O_NONBLOCK)
        };
        if fd < 0 {
            return Err(OpenError::Io(io::Error::last_os_error()));
        }
        // Advisory, and every radbeeper takes it -- which are exactly the
        // programs that would otherwise collide here.
        if unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            unsafe { libc::close(fd) };
            return Err(OpenError::Busy);
        }
        let port = Serial { fd, timeout };
        port.configure(baud)?;
        Ok(port)
    }

    fn configure(&self, baud: u32) -> Result<(), OpenError> {
        let speed = baud_constant(baud).ok_or_else(|| {
            OpenError::Io(io::Error::new(io::ErrorKind::InvalidInput, "baud"))
        })?;
        unsafe {
            let mut t: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(self.fd, &mut t) != 0 {
                return Err(OpenError::Io(io::Error::last_os_error()));
            }
            // cfmakeraw, spelled out: 8N1, no flow control, no echo, no line
            // discipline of any kind. The counter speaks bytes, not lines, and
            // a stray ICRNL turns a 0x0D in a count into a 0x0A and silently
            // corrupts every reading that happens to contain one.
            t.c_iflag &= !(libc::IGNBRK | libc::BRKINT | libc::PARMRK
                | libc::ISTRIP | libc::INLCR | libc::IGNCR | libc::ICRNL
                | libc::IXON);
            t.c_oflag &= !libc::OPOST;
            t.c_lflag &= !(libc::ECHO | libc::ECHONL | libc::ICANON
                | libc::ISIG | libc::IEXTEN);
            t.c_cflag &= !(libc::CSIZE | libc::PARENB | libc::CSTOPB
                | libc::CRTSCTS);
            t.c_cflag |= libc::CS8 | libc::CREAD | libc::CLOCAL;
            t.c_cc[libc::VMIN] = 0;
            t.c_cc[libc::VTIME] = 0;
            libc::cfsetispeed(&mut t, speed);
            libc::cfsetospeed(&mut t, speed);
            if libc::tcsetattr(self.fd, libc::TCSANOW, &t) != 0 {
                return Err(OpenError::Io(io::Error::last_os_error()));
            }
            libc::tcflush(self.fd, libc::TCIOFLUSH);
        }
        Ok(())
    }

    pub fn write_all(&self, data: &[u8]) -> io::Result<()> {
        let mut sent = 0;
        while sent < data.len() {
            let n = unsafe {
                libc::write(
                    self.fd,
                    data[sent..].as_ptr() as *const libc::c_void,
                    data.len() - sent,
                )
            };
            if n > 0 {
                sent += n as usize;
                continue;
            }
            let e = io::Error::last_os_error();
            if e.kind() == io::ErrorKind::WouldBlock {
                self.wait(true, self.timeout);
                continue;
            }
            return Err(e);
        }
        Ok(())
    }

    /// Exactly `count` bytes, or fewer if the deadline passes.
    pub fn read_exact_or_timeout(&self, count: usize, timeout: Duration) -> Vec<u8> {
        let deadline = Instant::now() + timeout;
        let mut out = Vec::with_capacity(count);
        while out.len() < count {
            let left = match deadline.checked_duration_since(Instant::now()) {
                Some(d) if !d.is_zero() => d,
                _ => break,
            };
            if !self.wait(false, left) {
                break;
            }
            let mut buf = vec![0u8; count - out.len()];
            let n = unsafe {
                libc::read(self.fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len())
            };
            if n > 0 {
                out.extend_from_slice(&buf[..n as usize]);
            } else if n == 0 {
                break;
            } else {
                let e = io::Error::last_os_error();
                if e.kind() == io::ErrorKind::WouldBlock {
                    continue;
                }
                break;
            }
        }
        out
    }

    fn wait(&self, writable: bool, timeout: Duration) -> bool {
        let mut p = libc::pollfd {
            fd: self.fd,
            events: if writable { libc::POLLOUT } else { libc::POLLIN },
            revents: 0,
        };
        let ms = timeout.as_millis().min(i32::MAX as u128) as libc::c_int;
        unsafe { libc::poll(&mut p, 1, ms) > 0 }
    }

    pub fn flush_input(&self) {
        unsafe { libc::tcflush(self.fd, libc::TCIFLUSH) };
    }
}

impl Drop for Serial {
    fn drop(&mut self) {
        unsafe { libc::close(self.fd) };
    }
}
