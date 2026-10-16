//! Comprobación de versiones nuevas en GitHub y reemplazo del ejecutable.
//!
//! Todo lo que toca la red corre en un hilo aparte y avisa a la interfaz por
//! un canal; la [`App`](crate::app::App) solo lee [`Updater::status`] y llama a
//! [`Updater::poll`] en cada cuadro.

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use eframe::egui::Context;
use sha2::{Digest, Sha256};

use crate::json::{self, Doc, Kind, NodeId};

const API_LATEST: &str = "https://api.github.com/repos/iscalexsoto/jsonviewer/releases/latest";
const USER_AGENT: &str =
    concat!("jsonviewer/", env!("CARGO_PKG_VERSION"), " (+https://github.com/iscalexsoto/jsonviewer)");
/// Terminación del asset portable de cada release.
const ASSET_SUFFIX: &str = "-x64.exe";
/// Entre comprobaciones automáticas (al iniciar) deben pasar al menos 24 h.
const CHECK_INTERVAL: u64 = 24 * 60 * 60;

pub const CURRENT: &str = env!("CARGO_PKG_VERSION");

/// Versión nueva publicada en GitHub.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Release {
    pub version: String,
    /// Página de la release (notas).
    pub notes_url: String,
    pub asset_name: String,
    pub asset_url: String,
    pub size: u64,
    /// SHA-256 esperado (hex en minúsculas) y de dónde salió.
    pub sha256: Option<(String, ShaSource)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShaSource {
    /// Campo `digest` que GitHub calcula para cada asset.
    Digest,
    /// Asset `<nombre>.sha256` publicado junto al ejecutable.
    Asset,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    Unchecked,
    Checking,
    UpToDate,
    Available(Release),
    Downloading {
        release: Release,
        received: u64,
        total: u64,
    },
    /// El ejecutable nuevo ya está en su lugar; falta reiniciar.
    Ready {
        version: String,
    },
    /// Se descargó y verificó pero no se pudo poner en el lugar del actual.
    Saved {
        version: String,
        path: PathBuf,
        reason: String,
    },
    Error(String),
}

/// Lo que el hilo de trabajo comunica a la interfaz.
enum Msg {
    Checked(Result<Option<Release>, String>),
    Progress { received: u64, total: u64 },
    Done(Result<PathBuf, (String, Option<PathBuf>)>),
}

/// Algo que la interfaz debe anunciar con un toast.
pub enum Event {
    UpToDate,
    Available(String),
    Ready(String),
    Saved(PathBuf),
    Error(String),
}

pub struct Updater {
    pub status: Status,
    rx: Option<Receiver<Msg>>,
    /// La comprobación en curso la pidió el usuario (se anuncia el resultado).
    manual: bool,
    /// El usuario eligió «Más tarde» tras instalar: solo se muestra un botón discreto.
    pub postponed: bool,
    /// Ya se lanzó el ejecutable nuevo; no repetirlo mientras se cierra la ventana.
    restarting: bool,
}

impl Default for Updater {
    fn default() -> Self {
        Self { status: Status::Unchecked, rx: None, manual: false, postponed: false, restarting: false }
    }
}

impl Updater {
    /// Borra el ejecutable anterior que dejó una actualización (`jsonviewer.old.exe`).
    pub fn cleanup() {
        if let Some(old) = std::env::current_exe().ok().and_then(|exe| old_exe_path(&exe)) {
            let _ = fs::remove_file(old);
        }
    }

    pub fn busy(&self) -> bool {
        self.rx.is_some()
    }

    /// Comprobación automática al iniciar: solo si está activada y pasó el intervalo.
    /// Devuelve `true` si se lanzó (para guardar la fecha en los ajustes).
    pub fn check_on_start(&mut self, ctx: &Context, enabled: bool, last_check: u64) -> bool {
        if !enabled || now().saturating_sub(last_check) < CHECK_INTERVAL {
            return false;
        }
        self.check(ctx, false);
        true
    }

    pub fn check(&mut self, ctx: &Context, manual: bool) {
        if self.busy() {
            self.manual |= manual;
            return;
        }
        self.manual = manual;
        self.status = Status::Checking;
        let (tx, rx) = mpsc::channel();
        self.rx = Some(rx);
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let r = fetch_latest().map(|rel| rel.filter(|r| newer(&r.version, CURRENT)));
            let _ = tx.send(Msg::Checked(r));
            ctx.request_repaint();
        });
    }

    pub fn download(&mut self, ctx: &Context) {
        let Status::Available(rel) = &self.status else { return };
        if self.busy() {
            return;
        }
        let rel = rel.clone();
        self.status = Status::Downloading { release: rel.clone(), received: 0, total: rel.size };
        let (tx, rx) = mpsc::channel();
        self.rx = Some(rx);
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let r = install(&rel, &tx, &ctx);
            let _ = tx.send(Msg::Done(r));
            ctx.request_repaint();
        });
    }

    /// Recoge lo que haya enviado el hilo de trabajo.
    pub fn poll(&mut self) -> Option<Event> {
        let rx = self.rx.take()?;
        let mut ev = None;
        let mut done = false;
        while let Ok(m) = rx.try_recv() {
            match m {
                Msg::Checked(Ok(Some(rel))) => {
                    let v = rel.version.clone();
                    self.status = Status::Available(rel);
                    done = true;
                    ev = self.manual.then_some(Event::Available(v));
                }
                Msg::Checked(Ok(None)) => {
                    self.status = Status::UpToDate;
                    done = true;
                    ev = self.manual.then_some(Event::UpToDate);
                }
                Msg::Checked(Err(e)) => {
                    self.status = Status::Error(e.clone());
                    done = true;
                    ev = self.manual.then_some(Event::Error(e));
                }
                Msg::Progress { received, total } => {
                    if let Status::Downloading { received: r, total: t, .. } = &mut self.status {
                        *r = received;
                        if total > 0 {
                            *t = total;
                        }
                    }
                }
                Msg::Done(r) => {
                    let Status::Downloading { release, .. } = std::mem::replace(&mut self.status, Status::Unchecked)
                    else {
                        unreachable!("solo se descarga desde Downloading")
                    };
                    done = true;
                    let version = release.version.clone();
                    match r {
                        Ok(_) => {
                            self.status = Status::Ready { version: version.clone() };
                            ev = Some(Event::Ready(version));
                        }
                        Err((reason, Some(path))) => {
                            self.status = Status::Saved { version, path: path.clone(), reason };
                            ev = Some(Event::Saved(path));
                        }
                        Err((reason, None)) => {
                            // Se puede volver a intentar.
                            self.status = Status::Available(release);
                            ev = Some(Event::Error(reason));
                        }
                    }
                }
            }
        }
        if !done {
            self.rx = Some(rx);
        }
        ev
    }

    /// Lanza el ejecutable nuevo (con los archivos abiertos) y cierra este.
    pub fn restart(&mut self, ctx: &Context, files: &[PathBuf]) -> Result<(), String> {
        if self.restarting {
            return Ok(());
        }
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        std::process::Command::new(&exe)
            .args(files)
            .spawn()
            .map_err(|e| format!("No se pudo iniciar {}: {e}", exe.display()))?;
        self.restarting = true;
        ctx.send_viewport_cmd(eframe::egui::ViewportCommand::Close);
        Ok(())
    }
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Segundos desde la época, para guardar la fecha de la última comprobación.
pub fn timestamp() -> u64 {
    now()
}

/// `1.4.0` > `1.3.1`; los tags pueden llevar `v` delante. Si no se entiende, no es más nueva.
pub fn newer(candidate: &str, current: &str) -> bool {
    match (parse_version(candidate), parse_version(current)) {
        (Some(a), Some(b)) => a > b,
        _ => false,
    }
}

fn parse_version(s: &str) -> Option<(u64, u64, u64)> {
    let s = s.trim().trim_start_matches(['v', 'V']);
    let s = s.split(['-', '+']).next()?;
    let mut it = s.split('.').map(|p| p.parse::<u64>().ok());
    let major = it.next()??;
    let minor = it.next().unwrap_or(Some(0))?;
    let patch = it.next().unwrap_or(Some(0))?;
    if it.next().is_some() {
        return None;
    }
    Some((major, minor, patch))
}

fn agent() -> ureq::Agent {
    let tls = ureq::tls::TlsConfig::builder()
        .provider(ureq::tls::TlsProvider::NativeTls)
        .root_certs(ureq::tls::RootCerts::PlatformVerifier)
        .build();
    ureq::Agent::config_builder()
        .tls_config(tls)
        .user_agent(USER_AGENT)
        .timeout_connect(Some(Duration::from_secs(15)))
        .timeout_recv_response(Some(Duration::from_secs(30)))
        .build()
        .into()
}

fn net_err(e: ureq::Error) -> String {
    match e {
        ureq::Error::StatusCode(403 | 429) => {
            "GitHub limitó las consultas desde esta red; prueba en unos minutos.".into()
        }
        ureq::Error::StatusCode(404) => "No hay ninguna release publicada.".into(),
        ureq::Error::StatusCode(code) => format!("GitHub respondió con el código {code}."),
        ureq::Error::HostNotFound | ureq::Error::ConnectionFailed => "Sin conexión con GitHub.".into(),
        ureq::Error::Timeout(_) => "GitHub tardó demasiado en responder.".into(),
        ureq::Error::Io(e) => format!("Error de red: {e}"),
        e => format!("No se pudo conectar: {e}"),
    }
}

/// Lee la última release. `None` si no tiene un asset `-x64.exe`.
fn fetch_latest() -> Result<Option<Release>, String> {
    let agent = agent();
    let mut resp = agent
        .get(API_LATEST)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .call()
        .map_err(net_err)?;
    let body = resp.body_mut().read_to_string().map_err(net_err)?;
    let doc = json::parse(&body).map_err(|e| format!("Respuesta de GitHub no válida: {e}"))?;
    let Some(rel) = parse_release(&doc).ok_or("La respuesta de GitHub no tiene el formato esperado.")? else {
        return Ok(None);
    };
    // Si no hay digest, busca un asset .sha256 y léelo.
    let rel = match (&rel.sha256, find_sha_asset(&doc, &rel.asset_name)) {
        (None, Some(url)) => {
            let sha = fetch_sha_asset(&agent, &url);
            Release { sha256: sha.map(|s| (s, ShaSource::Asset)), ..rel }
        }
        _ => rel,
    };
    Ok(Some(rel))
}

/// Campo de un objeto por nombre.
fn field(doc: &Doc, obj: NodeId, key: &str) -> Option<NodeId> {
    doc.node(obj).children.iter().copied().find(|&c| doc.key_of(doc.node(c)) == Some(key))
}

fn str_field<'a>(doc: &'a Doc, obj: NodeId, key: &str) -> Option<&'a str> {
    let n = doc.node(field(doc, obj, key)?);
    (n.kind == Kind::Str).then(|| doc.text_of(n))
}

/// `Some(None)` si la release existe pero no trae el asset `-x64.exe`.
fn parse_release(doc: &Doc) -> Option<Option<Release>> {
    let root = Doc::ROOT;
    if doc.node(root).kind != Kind::Object {
        return None;
    }
    let tag = str_field(doc, root, "tag_name")?;
    let notes_url = str_field(doc, root, "html_url")?;
    let assets = field(doc, root, "assets")?;
    let asset = doc
        .node(assets)
        .children
        .iter()
        .copied()
        .find(|&a| str_field(doc, a, "name").is_some_and(|n| n.ends_with(ASSET_SUFFIX) && !n.ends_with(".sha256")));
    let Some(asset) = asset else { return Some(None) };
    let name = str_field(doc, asset, "name")?;
    let url = str_field(doc, asset, "browser_download_url")?;
    let size = field(doc, asset, "size").map_or(0, |n| doc.text_of(doc.node(n)).parse().unwrap_or(0));
    let sha256 = str_field(doc, asset, "digest")
        .and_then(|d| d.strip_prefix("sha256:"))
        .and_then(hex_sha)
        .map(|s| (s, ShaSource::Digest));
    Some(Some(Release {
        version: tag.trim_start_matches(['v', 'V']).to_string(),
        notes_url: notes_url.to_string(),
        asset_name: name.to_string(),
        asset_url: url.to_string(),
        size,
        sha256,
    }))
}

fn find_sha_asset(doc: &Doc, asset_name: &str) -> Option<String> {
    let assets = field(doc, Doc::ROOT, "assets")?;
    let want = format!("{asset_name}.sha256");
    doc.node(assets)
        .children
        .iter()
        .copied()
        .find(|&a| str_field(doc, a, "name") == Some(want.as_str()))
        .and_then(|a| str_field(doc, a, "browser_download_url"))
        .map(str::to_string)
}

fn fetch_sha_asset(agent: &ureq::Agent, url: &str) -> Option<String> {
    let mut resp = agent.get(url).call().ok()?;
    let text = resp.body_mut().with_config().limit(4096).read_to_string().ok()?;
    text.split_whitespace().find_map(hex_sha)
}

/// 64 dígitos hexadecimales, normalizados a minúsculas.
fn hex_sha(s: &str) -> Option<String> {
    (s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())).then(|| s.to_ascii_lowercase())
}

fn old_exe_path(exe: &Path) -> Option<PathBuf> {
    let stem = exe.file_stem()?.to_str()?;
    Some(exe.with_file_name(format!("{stem}.old.exe")))
}

/// Descarga el asset junto al ejecutable, lo verifica y lo pone en su lugar.
///
/// `Err((motivo, Some(ruta)))`: el ejecutable nuevo quedó en `ruta` pero no se pudo
/// reemplazar el actual. `Err((motivo, None))`: no quedó nada utilizable.
fn install(rel: &Release, tx: &Sender<Msg>, ctx: &Context) -> Result<PathBuf, (String, Option<PathBuf>)> {
    let exe = std::env::current_exe().map_err(|e| (format!("No se encontró el ejecutable actual: {e}"), None))?;
    let dir = exe.parent().ok_or_else(|| ("El ejecutable no tiene carpeta.".to_string(), None))?;
    let part = dir.join(format!("{}.part", rel.asset_name));

    let r = download(rel, &part, tx, ctx);
    if let Err(e) = r {
        let _ = fs::remove_file(&part);
        return Err((e, None));
    }

    // Renombrar el actual a .old.exe (Windows permite renombrar un exe en ejecución).
    let old = old_exe_path(&exe).ok_or_else(|| ("Nombre de ejecutable no válido.".to_string(), None))?;
    let _ = fs::remove_file(&old);
    if let Err(e) = fs::rename(&exe, &old) {
        return Err(keep_aside(&part, dir, &rel.asset_name, format!("No se pudo apartar el ejecutable actual: {e}")));
    }
    if let Err(e) = fs::rename(&part, &exe) {
        // Intentar dejar todo como estaba.
        let _ = fs::rename(&old, &exe);
        return Err(keep_aside(&part, dir, &rel.asset_name, format!("No se pudo colocar el ejecutable nuevo: {e}")));
    }
    Ok(exe)
}

/// Deja el archivo descargado con su nombre definitivo junto al actual.
fn keep_aside(part: &Path, dir: &Path, name: &str, reason: String) -> (String, Option<PathBuf>) {
    let dest = dir.join(name);
    match fs::rename(part, &dest) {
        Ok(()) => (reason, Some(dest)),
        Err(_) => (reason, Some(part.to_path_buf())),
    }
}

fn download(rel: &Release, dest: &Path, tx: &Sender<Msg>, ctx: &Context) -> Result<(), String> {
    let mut resp = agent().get(&rel.asset_url).call().map_err(net_err)?;
    let total = resp.body().content_length().unwrap_or(rel.size);
    let mut file = fs::File::create(dest).map_err(|e| format!("No se pudo crear {}: {e}", dest.display()))?;
    let mut reader = resp.body_mut().as_reader();
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    let mut received = 0u64;
    let mut last_report = 0u64;
    loop {
        let n = reader.read(&mut buf).map_err(|e| format!("Se interrumpió la descarga: {e}"))?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n]).map_err(|e| format!("No se pudo escribir el archivo: {e}"))?;
        hasher.update(&buf[..n]);
        received += n as u64;
        if received - last_report >= 256 * 1024 {
            last_report = received;
            let _ = tx.send(Msg::Progress { received, total });
            ctx.request_repaint();
        }
    }
    file.flush().map_err(|e| e.to_string())?;
    drop(file);
    if received == 0 {
        return Err("La descarga llegó vacía.".into());
    }
    if total > 0 && received != total {
        return Err(format!("Descarga incompleta ({received} de {total} bytes)."));
    }
    if let Some((want, _)) = &rel.sha256 {
        let got = format!("{:x}", hasher.finalize());
        if &got != want {
            return Err("El archivo descargado no coincide con el SHA-256 publicado.".into());
        }
    }
    Ok(())
}

/// Texto del tooltip sobre la verificación del archivo.
pub fn sha_note(rel: &Release) -> String {
    match &rel.sha256 {
        Some((_, ShaSource::Digest)) => "Se verificará el SHA-256 que publica GitHub para el archivo.".into(),
        Some((_, ShaSource::Asset)) => "Se verificará el SHA-256 del archivo .sha256 de la release.".into(),
        None => "La release no publica SHA-256; el archivo no se verificará.".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert!(newer("v1.4.0", "1.3.1"));
        assert!(newer("1.10.0", "1.9.9"));
        assert!(newer("2", "1.99.99"));
        assert!(!newer("1.3.1", "1.3.1"));
        assert!(!newer("v1.3.0", "1.3.1"));
        assert!(!newer("1.4.0-beta", "1.4.0"));
        assert!(!newer("abc", "1.3.1"));
        assert!(!newer("1.4.0", "x"));
    }

    #[test]
    fn release_from_github_json() {
        let body = r#"{
          "tag_name": "v1.4.0",
          "html_url": "https://github.com/iscalexsoto/jsonviewer/releases/tag/v1.4.0",
          "assets": [
            {"name": "jsonviewer-1.4.0-x64.exe.sha256", "browser_download_url": "https://x/s", "size": 90},
            {"name": "jsonviewer-1.4.0-x64.exe", "browser_download_url": "https://x/e", "size": 8775680,
             "digest": "sha256:806096D02C6AD5B2D0587304D5FC7CF0291F66E5525F8394C30A9D92BB2DA8CD"}
          ]
        }"#;
        let doc = json::parse(body).unwrap();
        let rel = parse_release(&doc).unwrap().unwrap();
        assert_eq!(rel.version, "1.4.0");
        assert_eq!(rel.asset_name, "jsonviewer-1.4.0-x64.exe");
        assert_eq!(rel.asset_url, "https://x/e");
        assert_eq!(rel.size, 8775680);
        assert_eq!(
            rel.sha256,
            Some(("806096d02c6ad5b2d0587304d5fc7cf0291f66e5525f8394c30a9d92bb2da8cd".into(), ShaSource::Digest))
        );
        assert_eq!(find_sha_asset(&doc, &rel.asset_name).as_deref(), Some("https://x/s"));
    }

    #[test]
    fn release_without_asset() {
        let doc = json::parse(r#"{"tag_name":"v9.0.0","html_url":"u","assets":[]}"#).unwrap();
        assert_eq!(parse_release(&doc), Some(None));
        let doc = json::parse(r#"[1,2]"#).unwrap();
        assert_eq!(parse_release(&doc), None);
    }

    #[test]
    fn sha_text() {
        assert_eq!(
            "ABCDEF0123456789abcdef0123456789abcdef0123456789abcdef0123456789  jsonviewer-1.4.0-x64.exe"
                .split_whitespace()
                .find_map(hex_sha)
                .as_deref(),
            Some("abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789")
        );
        assert_eq!(hex_sha("abc"), None);
    }

    /// Contra el repositorio real (red): `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn real_release_and_download() {
        let rel = fetch_latest().expect("consulta a GitHub").expect("asset -x64.exe");
        assert!(parse_version(&rel.version).is_some(), "tag {}", rel.version);
        assert!(rel.asset_name.ends_with(ASSET_SUFFIX));
        assert!(rel.notes_url.starts_with("https://github.com/iscalexsoto/jsonviewer/releases/"));
        assert!(rel.sha256.is_some(), "GitHub publica digest para los assets");
        let dir = std::env::temp_dir().join("jsonviewer-update-test");
        fs::create_dir_all(&dir).unwrap();
        let dest = dir.join(&rel.asset_name);
        let (tx, rx) = mpsc::channel();
        download(&rel, &dest, &tx, &Context::default()).expect("descarga y SHA-256");
        assert_eq!(fs::metadata(&dest).unwrap().len(), rel.size);
        assert!(rx.try_iter().any(|m| matches!(m, Msg::Progress { .. })));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn old_name() {
        let p = old_exe_path(Path::new(r"C:\Apps\jsonviewer.exe")).unwrap();
        assert_eq!(p, PathBuf::from(r"C:\Apps\jsonviewer.old.exe"));
    }
}
