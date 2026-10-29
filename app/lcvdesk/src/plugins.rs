//! Complementos: programas propios que LCVDesk conecta entre los dos equipos (por ahora, IGP_Soft).
//!
//! LCVDesk solo hace de túnel: en cada equipo une una tubería con nombre local y copia sus bytes tal
//! cual por la sesión (`PluginData`), en orden y en ambos sentidos a la vez. No interpreta nada.
//! Contrato con IGP_Soft: `docs/IGP_SOFT.md` (y su `IGP_Soft-plan-tunel.xml`, versión 4).
//!
//! Las tuberías se abren con `FILE_FLAG_OVERLAPPED`: con un handle síncrono, un `ReadFile` en espera
//! bloquea cualquier `WriteFile` desde otro hilo, y el túnel dejaría de ser dúplex.

use lcv_proto::{Message, PluginInfo};
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use tracing::{info, warn};
use windows::Win32::Foundation::{
    CloseHandle, ERROR_ACCESS_DENIED, ERROR_BROKEN_PIPE, ERROR_FILE_NOT_FOUND, ERROR_IO_PENDING, ERROR_NO_DATA,
    ERROR_OPERATION_ABORTED, ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED, ERROR_PIPE_NOT_CONNECTED, ERROR_SUCCESS,
    GENERIC_READ, GENERIC_WRITE, HANDLE, HLOCAL, INVALID_HANDLE_VALUE, LocalFree, WAIT_OBJECT_0,
};
use windows::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows::Win32::Security::{GetTokenInformation, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER, TokenUser};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, FILE_SHARE_NONE, OPEN_EXISTING, PIPE_ACCESS_DUPLEX,
    ReadFile, SECURITY_IDENTIFICATION, SECURITY_SQOS_PRESENT, WriteFile,
};
use windows::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE,
    PIPE_WAIT, WaitNamedPipeW,
};
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ, RRF_SUBKEY_WOW6464KEY, RegGetValueW,
};
use windows::Win32::System::RemoteDesktop::ProcessIdToSessionId;
use windows::Win32::System::Threading::{
    CreateEventW, GetCurrentProcess, GetCurrentProcessId, OpenProcessToken, WaitForSingleObject,
};
use windows::core::{HSTRING, PWSTR};

/// Identificador de IGP_Soft en los mensajes `Plugin*`.
pub const IGP_SOFT: &str = "igp_soft";
/// Página de descarga que se ofrece si falta IGP_Soft o las versiones no coinciden.
pub const IGP_SOFT_DOWNLOAD: &str = "https://github.com/LCV2107/IGP_Soft/releases/latest";
/// Plazo para que una tubería se conecte (contrato: 10 s en los dos lados).
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Tamaño máximo de cada `PluginData`. IGP_Soft limita cada línea a 1 MB; aquí no se parten líneas,
/// solo se copian bloques de bytes en orden.
const CHUNK: usize = 64 * 1024;

/// Un complemento instalado en este equipo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Installed {
    pub exe: PathBuf,
    pub version: String,
}

/// Busca el complemento `id` en este equipo. IGP_Soft: primero la copia instalada (HKLM, la escribe
/// el MSI), luego la portable (HKCU, la escribe al abrirse). Si el registro apunta a un `.exe` que ya
/// no existe, cuenta como no instalado.
pub fn detect(id: &str) -> Option<Installed> {
    if id != IGP_SOFT {
        return None;
    }
    let installed = reg_string(HKEY_LOCAL_MACHINE, "Software\\IGP_Soft", "Carpeta").and_then(|dir| {
        let version = reg_string(HKEY_LOCAL_MACHINE, "Software\\IGP_Soft", "Version")?;
        Some(Installed { exe: PathBuf::from(dir).join("IGP_Soft.exe"), version })
    });
    let portable = || {
        let exe = reg_string(HKEY_CURRENT_USER, "Software\\IGP_Soft", "Ruta")?;
        let version = reg_string(HKEY_CURRENT_USER, "Software\\IGP_Soft", "Version")?;
        Some(Installed { exe: PathBuf::from(exe), version })
    };
    installed.filter(|i| i.exe.is_file()).or_else(|| portable().filter(|i| i.exe.is_file()))
}

/// Complementos de este equipo para `PluginList` (`version` vacía si no está instalado).
pub fn local_list() -> Vec<PluginInfo> {
    vec![PluginInfo { id: IGP_SOFT.into(), version: detect(IGP_SOFT).map(|i| i.version).unwrap_or_default() }]
}

/// Contrato: misma versión en los dos equipos, texto exacto sin distinguir mayúsculas (incluido -exp.N).
pub fn same_version(a: &str, b: &str) -> bool {
    !a.is_empty() && a.eq_ignore_ascii_case(b)
}

/// Nombre de la tubería de IGP_Soft en el equipo controlado: `IGP_Soft.Control.v1.<SessionId>`.
pub fn igp_control_pipe() -> String {
    let mut session = 0u32;
    if let Err(e) = unsafe { ProcessIdToSessionId(GetCurrentProcessId(), &mut session) } {
        warn!("complementos: no se pudo leer la sesión de Windows ({e}); se usa la 0");
    }
    format!("IGP_Soft.Control.v1.{session}")
}

/// Nombre aleatorio para la tubería del equipo que controla: `LCVDesk.IGP.<32 hex>`.
pub fn random_pipe_name() -> String {
    let hex: String = crate::auth::random_nonce()[..16].iter().map(|b| format!("{b:02x}")).collect();
    format!("LCVDesk.IGP.{hex}")
}

/// Abre IGP_Soft con los argumentos del contrato (`--controlado`, o `--remoto --tubo=<nombre>`).
/// La ruta sale siempre del registro de este equipo, nunca de la sesión.
pub fn launch(exe: &std::path::Path, args: &[String]) -> io::Result<()> {
    use std::process::Stdio;
    info!("complementos: abriendo {} {}", exe.display(), args.join(" "));
    // Sin heredar nada de LCVDesk: IGP_Soft sigue abierto por su cuenta (p. ej. --controlado).
    std::process::Command::new(exe)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
}

fn reg_string(root: HKEY, subkey: &str, value: &str) -> Option<String> {
    let (subkey, value) = (HSTRING::from(subkey), HSTRING::from(value));
    // HKLM en su vista de 64 bits (el MSI de IGP_Soft es de 64 bits).
    let flags = RRF_RT_REG_SZ | RRF_SUBKEY_WOW6464KEY;
    let mut size = 0u32;
    if unsafe { RegGetValueW(root, &subkey, &value, flags, None, None, Some(&mut size)) } != ERROR_SUCCESS || size < 2 {
        return None;
    }
    let mut buf = vec![0u16; size as usize / 2];
    let r = unsafe {
        RegGetValueW(root, &subkey, &value, flags, None, Some(buf.as_mut_ptr().cast()), Some(&mut size))
    };
    if r != ERROR_SUCCESS {
        return None;
    }
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    Some(String::from_utf16_lossy(&buf[..len])).filter(|s| !s.trim().is_empty())
}

// ---------------------------------------------------------------------------------------------
// Tuberías con nombre en modo overlapped
// ---------------------------------------------------------------------------------------------

/// Código Win32 de un error de `windows` (HRESULT_FROM_WIN32 → código original).
fn win32(e: &windows::core::Error) -> u32 {
    let h = e.code().0 as u32;
    if h & 0xFFFF_0000 == 0x8007_0000 { h & 0xFFFF } else { h }
}

fn io_err(e: &windows::core::Error) -> io::Error {
    io::Error::from_raw_os_error(win32(e) as i32)
}

/// Evento manual para un `OVERLAPPED`; se cierra al soltarlo.
struct Event(HANDLE);

impl Event {
    fn new() -> io::Result<Self> {
        unsafe { CreateEventW(None, true, false, None) }.map(Self).map_err(|e| io_err(&e))
    }
}

impl Drop for Event {
    fn drop(&mut self) {
        let _ = unsafe { CloseHandle(self.0) };
    }
}

/// Un extremo de tubería. Se puede leer desde un hilo y escribir desde otro a la vez.
pub struct Pipe {
    handle: HANDLE,
    server: bool,
    closed: AtomicBool,
}

// El handle es de Windows y las operaciones overlapped admiten hilos distintos sobre él.
unsafe impl Send for Pipe {}
unsafe impl Sync for Pipe {}

impl Pipe {
    /// Se conecta como cliente a `\\.\pipe\<name>`, reintentando hasta `timeout` mientras la tubería
    /// no exista o esté ocupada. «Acceso denegado» se devuelve enseguida (`PermissionDenied`).
    pub fn connect(name: &str, timeout: Duration) -> io::Result<Self> {
        let path = HSTRING::from(format!(r"\\.\pipe\{name}"));
        let deadline = Instant::now() + timeout;
        loop {
            let r = unsafe {
                CreateFileW(
                    &path,
                    (GENERIC_READ | GENERIC_WRITE).0,
                    FILE_SHARE_NONE,
                    None,
                    OPEN_EXISTING,
                    // Sin dejar que el servidor nos suplante más allá de identificarnos.
                    FILE_FLAG_OVERLAPPED | SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION,
                    None,
                )
            };
            let e = match r {
                Ok(handle) => return Ok(Self { handle, server: false, closed: AtomicBool::new(false) }),
                Err(e) => e,
            };
            let code = win32(&e);
            if code == ERROR_ACCESS_DENIED.0 {
                return Err(io::Error::new(io::ErrorKind::PermissionDenied, format!("acceso denegado a la tubería {name}")));
            }
            if Instant::now() >= deadline {
                return Err(io::Error::new(io::ErrorKind::TimedOut, format!("la tubería {name} no respondió ({e})")));
            }
            if code == ERROR_PIPE_BUSY.0 {
                let _ = unsafe { WaitNamedPipeW(&path, 250) };
            } else if code == ERROR_FILE_NOT_FOUND.0 {
                thread::sleep(Duration::from_millis(200));
            } else {
                return Err(io_err(&e));
            }
        }
    }

    /// Crea la tubería servidor `\\.\pipe\<name>`: una sola instancia, modo byte, dúplex, solo
    /// clientes locales y solo el usuario actual.
    pub fn create_server(name: &str) -> io::Result<Self> {
        let path = HSTRING::from(format!(r"\\.\pipe\{name}"));
        let sid = current_user_sid()?;
        let sddl = HSTRING::from(format!("D:P(A;;GA;;;{sid})"));
        let mut sd = PSECURITY_DESCRIPTOR::default();
        unsafe { ConvertStringSecurityDescriptorToSecurityDescriptorW(&sddl, SDDL_REVISION_1, &mut sd, None) }
            .map_err(|e| io_err(&e))?;
        let sa = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: sd.0,
            bInheritHandle: false.into(),
        };
        let handle = unsafe {
            CreateNamedPipeW(
                &path,
                PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED | FILE_FLAG_FIRST_PIPE_INSTANCE,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                1,
                CHUNK as u32,
                CHUNK as u32,
                0,
                Some(&sa),
            )
        };
        let _ = unsafe { LocalFree(Some(HLOCAL(sd.0))) };
        if handle == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { handle, server: true, closed: AtomicBool::new(false) })
    }

    /// Espera a que un cliente se conecte a la tubería servidor, como mucho `timeout`. Se abandona
    /// antes si `cancel` se activa (sesión cerrada o complemento cerrado).
    pub fn accept(&self, timeout: Duration, cancel: &AtomicBool) -> io::Result<()> {
        let ev = Event::new()?;
        let mut ov = OVERLAPPED { hEvent: ev.0, ..Default::default() };
        match unsafe { ConnectNamedPipe(self.handle, Some(&mut ov)) } {
            Ok(()) => return Ok(()),
            Err(e) if win32(&e) == ERROR_PIPE_CONNECTED.0 => return Ok(()),
            Err(e) if win32(&e) == ERROR_IO_PENDING.0 => {}
            Err(e) => return Err(io_err(&e)),
        }
        let deadline = Instant::now() + timeout;
        loop {
            if unsafe { WaitForSingleObject(ev.0, 200) } == WAIT_OBJECT_0 {
                let mut n = 0u32;
                return unsafe { GetOverlappedResult(self.handle, &ov, &mut n, false) }.map_err(|e| io_err(&e));
            }
            if cancel.load(Ordering::Relaxed) || Instant::now() >= deadline {
                // Cancelar y esperar a que Windows suelte el OVERLAPPED antes de salir.
                let _ = unsafe { CancelIoEx(self.handle, Some(&ov)) };
                let mut n = 0u32;
                let _ = unsafe { GetOverlappedResult(self.handle, &ov, &mut n, true) };
                return Err(io::Error::new(io::ErrorKind::TimedOut, "nadie se conectó a la tubería"));
            }
        }
    }

    /// Lee lo disponible (bloquea hasta que haya algo). `Ok(0)` = el otro extremo cerró.
    pub fn read(&self, buf: &mut [u8]) -> io::Result<usize> {
        let ev = Event::new()?;
        let mut ov = OVERLAPPED { hEvent: ev.0, ..Default::default() };
        if let Err(e) = unsafe { ReadFile(self.handle, Some(buf), None, Some(&mut ov)) } {
            match win32(&e) {
                c if c == ERROR_IO_PENDING.0 => {}
                c if is_closed_code(c) => return Ok(0),
                _ => return Err(io_err(&e)),
            }
        }
        let mut n = 0u32;
        match unsafe { GetOverlappedResult(self.handle, &ov, &mut n, true) } {
            Ok(()) => Ok(n as usize),
            Err(e) if is_closed_code(win32(&e)) => Ok(0),
            Err(e) => Err(io_err(&e)),
        }
    }

    /// Escribe todo el bloque (en varias operaciones si hace falta).
    pub fn write_all(&self, mut data: &[u8]) -> io::Result<()> {
        let ev = Event::new()?;
        while !data.is_empty() {
            let mut ov = OVERLAPPED { hEvent: ev.0, ..Default::default() };
            if let Err(e) = unsafe { WriteFile(self.handle, Some(data), None, Some(&mut ov)) }
                && win32(&e) != ERROR_IO_PENDING.0
            {
                return Err(io_err(&e));
            }
            let mut n = 0u32;
            unsafe { GetOverlappedResult(self.handle, &ov, &mut n, true) }.map_err(|e| io_err(&e))?;
            if n == 0 {
                return Err(io::Error::new(io::ErrorKind::WriteZero, "la tubería no aceptó datos"));
            }
            data = &data[n as usize..];
        }
        Ok(())
    }

    /// Corta la tubería: despierta las lecturas y escrituras en curso. Se puede llamar varias veces.
    pub fn close(&self) {
        if self.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        let _ = unsafe { CancelIoEx(self.handle, None) };
        if self.server {
            let _ = unsafe { DisconnectNamedPipe(self.handle) };
        }
    }
}

impl Drop for Pipe {
    fn drop(&mut self) {
        self.close();
        let _ = unsafe { CloseHandle(self.handle) };
    }
}

fn is_closed_code(c: u32) -> bool {
    [ERROR_BROKEN_PIPE.0, ERROR_PIPE_NOT_CONNECTED.0, ERROR_NO_DATA.0, ERROR_OPERATION_ABORTED.0].contains(&c)
}

/// SID del usuario de este proceso en texto (`S-1-5-21-…`), para la ACL de la tubería servidor.
fn current_user_sid() -> io::Result<String> {
    unsafe {
        let mut token = HANDLE::default();
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).map_err(|e| io_err(&e))?;
        let mut len = 0u32;
        let _ = GetTokenInformation(token, TokenUser, None, 0, &mut len);
        // Búfer de u64 para que TOKEN_USER quede bien alineado.
        let mut buf = vec![0u64; (len as usize).div_ceil(8)];
        let r = GetTokenInformation(token, TokenUser, Some(buf.as_mut_ptr().cast()), len, &mut len);
        let _ = CloseHandle(token);
        r.map_err(|e| io_err(&e))?;
        let user = &*(buf.as_ptr() as *const TOKEN_USER);
        let mut text = PWSTR::null();
        ConvertSidToStringSidW(user.User.Sid, &mut text).map_err(|e| io_err(&e))?;
        let sid = text.to_string().map_err(|_| io::Error::other("SID no válido"));
        let _ = LocalFree(Some(HLOCAL(text.0.cast())));
        sid
    }
}

// ---------------------------------------------------------------------------------------------
// Canal: une una tubería con la sesión
// ---------------------------------------------------------------------------------------------

/// Un complemento abierto en un canal de la sesión. Lo que llega de la red se encola (sin límite:
/// el hilo que lee la red atiende también el ratón y el teclado y no debe quedarse esperando) y un
/// hilo propio lo escribe en la tubería; otro hilo copia la tubería hacia la red.
pub struct Channel {
    pub id: u32,
    pub plugin: String,
    to_pipe: Sender<Vec<u8>>,
    to_pipe_rx: Mutex<Option<Receiver<Vec<u8>>>>,
    pipe: Mutex<Option<Arc<Pipe>>>,
    closed: AtomicBool,
    sent: AtomicU64,
    received: AtomicU64,
}

impl Channel {
    pub fn new(id: u32, plugin: &str) -> Arc<Self> {
        let (to_pipe, rx) = mpsc::channel();
        Arc::new(Self {
            id,
            plugin: plugin.into(),
            to_pipe,
            to_pipe_rx: Mutex::new(Some(rx)),
            pipe: Mutex::new(None),
            closed: AtomicBool::new(false),
            sent: AtomicU64::new(0),
            received: AtomicU64::new(0),
        })
    }

    /// Bytes recibidos de la red para la tubería. Se guardan aunque la tubería aún no esté conectada.
    pub fn push(&self, data: Vec<u8>) {
        self.received.fetch_add(data.len() as u64, Ordering::Relaxed);
        let _ = self.to_pipe.send(data);
    }

    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    /// Bandera de cierre, para abandonar esperas (`Pipe::accept`).
    pub fn cancel_flag(&self) -> &AtomicBool {
        &self.closed
    }

    /// Cierra el canal y su tubería. Devuelve `true` si estaba abierto (solo la primera vez).
    pub fn close(&self) -> bool {
        let was_open = !self.closed.swap(true, Ordering::SeqCst);
        if let Some(p) = self.pipe.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            p.close();
        }
        if was_open {
            info!(
                "complemento {} (canal {}) cerrado: {} B hacia la red, {} B hacia la tubería",
                self.plugin,
                self.id,
                self.sent.load(Ordering::Relaxed),
                self.received.load(Ordering::Relaxed)
            );
        }
        was_open
    }

    /// Empieza a copiar entre `pipe` y la sesión. `send` entrega un mensaje a la red y devuelve
    /// `false` si la sesión ya no existe. Cuando la tubería se cierra por su lado, se avisa al otro
    /// equipo con `PluginClose` y se llama a `on_close`.
    pub fn start(
        self: &Arc<Self>,
        pipe: Pipe,
        send: impl Fn(Message) -> bool + Send + Sync + 'static,
        on_close: impl FnOnce() + Send + 'static,
    ) {
        let pipe = Arc::new(pipe);
        *self.pipe.lock().unwrap_or_else(|e| e.into_inner()) = Some(pipe.clone());
        // Si se cerró mientras se conectaba, no se arranca nada.
        if self.is_closed() {
            pipe.close();
            on_close();
            return;
        }
        let rx = self.to_pipe_rx.lock().unwrap_or_else(|e| e.into_inner()).take();
        let send = Arc::new(send);

        // Red → tubería.
        if let Some(rx) = rx {
            let (ch, pipe) = (self.clone(), pipe.clone());
            thread::Builder::new()
                .name("plugin-to-pipe".into())
                .spawn(move || {
                    while !ch.is_closed() {
                        match rx.recv_timeout(Duration::from_millis(250)) {
                            Ok(data) => {
                                if let Err(e) = pipe.write_all(&data) {
                                    warn!("complemento {} (canal {}): no se pudo escribir en la tubería: {e}", ch.plugin, ch.id);
                                    pipe.close();
                                    break;
                                }
                            }
                            Err(mpsc::RecvTimeoutError::Timeout) => {}
                            Err(mpsc::RecvTimeoutError::Disconnected) => break,
                        }
                    }
                })
                .ok();
        }

        // Tubería → red.
        let ch = self.clone();
        thread::Builder::new()
            .name("plugin-from-pipe".into())
            .spawn(move || {
                let mut buf = vec![0u8; CHUNK];
                loop {
                    match pipe.read(&mut buf) {
                        Ok(0) => break,
                        Ok(n) => {
                            ch.sent.fetch_add(n as u64, Ordering::Relaxed);
                            if !send(Message::PluginData { channel: ch.id, data: buf[..n].to_vec() }) {
                                break;
                            }
                        }
                        Err(e) => {
                            warn!("complemento {} (canal {}): error al leer la tubería: {e}", ch.plugin, ch.id);
                            break;
                        }
                    }
                }
                // Si el cierre vino de este lado (el programa cerró la tubería), se avisa al otro equipo.
                if ch.close() {
                    send(Message::PluginClose { channel: ch.id });
                }
                on_close();
            })
            .ok();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_exactly_ignoring_case() {
        assert!(same_version("1.0.2-exp.2", "1.0.2-EXP.2"));
        assert!(!same_version("1.0.2-exp.2", "1.0.2-exp.3"));
        assert!(!same_version("1.0.2", "1.0.2-exp.2"));
        assert!(!same_version("", ""));
    }

    #[test]
    fn pipe_name_has_session() {
        assert!(igp_control_pipe().starts_with("IGP_Soft.Control.v1."));
        let a = random_pipe_name();
        assert!(a.starts_with("LCVDesk.IGP.") && !a.contains(' '));
        assert_ne!(a, random_pipe_name());
    }

    /// Lectura y escritura simultáneas en las dos puntas: cada lado envía 3 MB mientras recibe los
    /// 3 MB del otro. Con handles síncronos esto se bloquearía.
    #[test]
    fn pipes_are_full_duplex() {
        let name = random_pipe_name();
        let server = Arc::new(Pipe::create_server(&name).unwrap());
        let s2 = server.clone();
        let acceptor = thread::spawn(move || s2.accept(Duration::from_secs(5), &AtomicBool::new(false)));
        let client = Arc::new(Pipe::connect(&name, Duration::from_secs(5)).unwrap());
        acceptor.join().unwrap().unwrap();

        const TOTAL: usize = 3 * 1024 * 1024;
        let pump = |from: Arc<Pipe>, to: Arc<Pipe>, byte: u8| {
            let writer = thread::spawn(move || from.write_all(&vec![byte; TOTAL]).unwrap());
            let reader = thread::spawn(move || {
                let (mut got, mut buf) = (0usize, vec![0u8; CHUNK]);
                while got < TOTAL {
                    let n = to.read(&mut buf).unwrap();
                    assert!(n > 0, "cerrado antes de tiempo");
                    assert!(buf[..n].iter().all(|&b| b == byte));
                    got += n;
                }
            });
            (writer, reader)
        };
        let (w1, r1) = pump(server.clone(), client.clone(), b'a');
        let (w2, r2) = pump(client.clone(), server.clone(), b'b');
        for h in [w1, r1, w2, r2] {
            h.join().unwrap();
        }
    }

    /// Una línea de 1 MB llega entera y el cierre de un lado se ve como fin en el otro.
    #[test]
    fn big_line_and_close() {
        let name = random_pipe_name();
        let server = Arc::new(Pipe::create_server(&name).unwrap());
        let s2 = server.clone();
        let acceptor = thread::spawn(move || s2.accept(Duration::from_secs(5), &AtomicBool::new(false)));
        let client = Pipe::connect(&name, Duration::from_secs(5)).unwrap();
        acceptor.join().unwrap().unwrap();

        let mut line = vec![b'x'; 1024 * 1024 - 1];
        line.push(b'\n');
        let expected = line.clone();
        let s3 = server.clone();
        let reader = thread::spawn(move || {
            let (mut got, mut buf) = (Vec::new(), vec![0u8; CHUNK]);
            loop {
                let n = s3.read(&mut buf).unwrap();
                if n == 0 {
                    break got;
                }
                got.extend_from_slice(&buf[..n]);
            }
        });
        client.write_all(&line).unwrap();
        drop(client);
        assert_eq!(reader.join().unwrap(), expected);
    }

    /// `accept` respeta el plazo y la cancelación.
    #[test]
    fn accept_times_out() {
        let server = Pipe::create_server(&random_pipe_name()).unwrap();
        let t = Instant::now();
        assert!(server.accept(Duration::from_millis(400), &AtomicBool::new(false)).is_err());
        assert!(t.elapsed() >= Duration::from_millis(350));
        assert!(server.accept(Duration::from_secs(10), &AtomicBool::new(true)).is_err());
    }

    /// Prueba real con el IGP_Soft instalado en este equipo (no corre por defecto):
    /// `cargo test -p lcvdesk igp_soft_end_to_end -- --ignored --nocapture`.
    /// Une el panel `--remoto` con el IGP_Soft de este equipo a través de dos canales, como harían
    /// el visor y el host, y comprueba el saludo y el primer estado.
    #[test]
    #[ignore]
    fn igp_soft_end_to_end() {
        let igp = detect(IGP_SOFT).expect("IGP_Soft no está instalado en este equipo");
        println!("IGP_Soft {} en {}", igp.version, igp.exe.display());

        // Lado controlado: la tubería de IGP_Soft (abriéndolo con --controlado si hace falta).
        let control = match Pipe::connect(&igp_control_pipe(), Duration::from_millis(500)) {
            Ok(p) => p,
            Err(_) => {
                launch(&igp.exe, &["--controlado".into()]).unwrap();
                Pipe::connect(&igp_control_pipe(), CONNECT_TIMEOUT).expect("IGP_Soft --controlado no abrió su tubería")
            }
        };
        // Lado que controla: tubería propia + panel remoto.
        let name = random_pipe_name();
        let server = Pipe::create_server(&name).unwrap();
        launch(&igp.exe, &["--remoto".into(), format!("--tubo={name}")]).unwrap();
        server.accept(CONNECT_TIMEOUT, &AtomicBool::new(false)).expect("el panel remoto no se conectó");

        // Dos canales unidos en memoria: lo que sale de uno entra en el otro (como la sesión).
        let host = Channel::new(1, IGP_SOFT);
        let viewer = Channel::new(1, IGP_SOFT);
        let log = Arc::new(Mutex::new(Vec::<u8>::new()));
        let (h, l) = (host.clone(), log.clone());
        let (closed_tx, closed_rx) = mpsc::channel();
        let closed_tx2 = closed_tx.clone();
        viewer.start(
            server,
            move |m| {
                if let Message::PluginData { data, .. } = m {
                    h.push(data);
                }
                true
            },
            move || {
                let _ = closed_tx.send("visor");
            },
        );
        let v = viewer.clone();
        host.start(
            control,
            move |m| {
                if let Message::PluginData { data, .. } = m {
                    l.lock().unwrap().extend_from_slice(&data);
                    v.push(data);
                }
                true
            },
            move || {
                let _ = closed_tx2.send("host");
            },
        );

        // Esperar el saludo correcto y el primer estado del equipo controlado.
        let t = Instant::now();
        let (mut hello_ok, mut state) = (false, false);
        while t.elapsed() < Duration::from_secs(20) && !(hello_ok && state) {
            thread::sleep(Duration::from_millis(200));
            let text = String::from_utf8_lossy(&log.lock().unwrap()).into_owned();
            hello_ok = text.lines().any(|l| l.contains("\"id\":1") && l.contains("\"ok\":true"));
            state = text.lines().any(|l| l.contains("\"tipo\":\"estado\""));
        }
        let text = String::from_utf8_lossy(&log.lock().unwrap()).into_owned();
        for l in text.lines().take(3) {
            println!("← {}", if l.len() > 160 { &l[..160] } else { l });
        }
        assert!(hello_ok, "IGP_Soft no aceptó el saludo:\n{text}");
        assert!(state, "no llegó el estado del equipo controlado");

        // Latidos: tras 7 s la conexión sigue viva (pasan en los dos sentidos sin filtrarse).
        thread::sleep(Duration::from_secs(7));
        assert!(!host.is_closed() && !viewer.is_closed(), "la conexión se cayó");
        let beats = String::from_utf8_lossy(&log.lock().unwrap()).matches("\"latido\"").count();
        println!("latidos del controlado recibidos: {beats}");
        assert!(beats >= 2);

        // Cerrar el lado del host: el panel remoto debe notarlo (y el controlado desbloquearse).
        host.close();
        println!("cerrado el lado controlado; el panel remoto reintentará 10 s y se cerrará");
        let _ = closed_rx.recv_timeout(Duration::from_secs(5));
        viewer.close();
    }

    /// Lo mismo, pero por una sesión real de LCVDesk: host y visor de este equipo conectados por TCP
    /// (127.0.0.1), con el botón «Abrir IGP_Soft» del visor. Acepta solo el diálogo de conexión.
    /// `cargo test -p lcvdesk igp_soft_through_session -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn igp_soft_through_session() {
        use crate::viewer::{PluginState, Status, Viewer};
        use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, IDYES, PostMessageW, WM_COMMAND};
        use windows::core::w;

        let _ = tracing_subscriber::fmt().with_test_writer().with_thread_names(true).try_init();
        let port = 47_170;
        let _host = crate::host::Host::start(port);
        thread::sleep(Duration::from_millis(500));

        // Pulsa «Sí» en el diálogo de aceptación del host en cuanto aparezca.
        thread::spawn(|| {
            for _ in 0..100 {
                if let Ok(hwnd) = unsafe { FindWindowW(None, w!("LCVDesk — Solicitud de conexión")) } {
                    let _ = unsafe {
                        PostMessageW(Some(hwnd), WM_COMMAND, windows::Win32::Foundation::WPARAM(IDYES.0 as usize), Default::default())
                    };
                    return;
                }
                thread::sleep(Duration::from_millis(100));
            }
        });

        let viewer = Viewer::connect(format!("127.0.0.1:{port}"), "prueba".into(), None, eframe::egui::Context::default());
        let wait = |what: &str, secs: u64, ok: &dyn Fn() -> bool| {
            let t = Instant::now();
            while !ok() {
                assert!(t.elapsed() < Duration::from_secs(secs), "tiempo agotado esperando: {what}");
                thread::sleep(Duration::from_millis(100));
            }
            println!("ok: {what} ({:.1} s)", t.elapsed().as_secs_f32());
        };
        wait("sesión aceptada", 20, &|| matches!(viewer.shared().status, Status::Streaming));
        wait("lista de complementos del host", 5, &|| viewer.shared().remote_plugins.is_some());
        println!("complementos del host: {:?}", viewer.shared().remote_plugins);
        assert!(viewer.shared().plugin_supported);

        viewer.open_plugin(IGP_SOFT);
        wait("IGP_Soft abierto en los dos lados", 25, &|| {
            let st = viewer.shared().plugin_state.get(IGP_SOFT).cloned();
            if let Some(PluginState::Error(e)) = &st {
                panic!("error al abrir: {e}");
            }
            st == Some(PluginState::Open)
        });
        // Con el saludo hecho, el panel remoto solo sigue conectado si los latidos cruzan el túnel:
        // sin datos durante 10 s daría la conexión por perdida y cerraría su tubería.
        thread::sleep(Duration::from_secs(14));
        assert_eq!(viewer.shared().plugin_state.get(IGP_SOFT), Some(&PluginState::Open), "el panel remoto se desconectó");
        println!("ok: sigue conectado tras 14 s (los latidos cruzan el túnel)");

        viewer.close_plugin(IGP_SOFT);
        thread::sleep(Duration::from_secs(1));
        viewer.disconnect();
        thread::sleep(Duration::from_secs(1));
    }
}
