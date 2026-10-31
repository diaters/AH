//! 服务端证书与配对链接。未配置证书时由实例自管私有 CA，客户端凭链接中的指纹信任它。
use std::{
    fs,
    io::Write,
    net::{IpAddr, SocketAddr},
    path::{Path, PathBuf},
};

use anyhow::Context;
use base64::Engine;
use chrono::{Datelike, Duration, Utc};
use rcgen::{
    BasicConstraints, CertificateParams, DistinguishedName, DnType, ExtendedKeyUsagePurpose, IsCa,
    KeyPair, KeyUsagePurpose,
};
use sha2::{Digest, Sha256};

use super::ClientConfig;

const CA_NAME: &str = "Harness Client CA";

pub struct ServerTls {
    pub cert_pem: String,
    pub key_pem: String,
    /// 仅自管模式提供：CA 证书 DER 的 SHA-256 指纹（小写十六进制）。
    pub ca_fingerprint: Option<String>,
}

fn managed_dir(config: &ClientConfig) -> PathBuf {
    config
        .state_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
        .to_path_buf()
}

fn ca_params() -> anyhow::Result<CertificateParams> {
    let mut params = CertificateParams::new(Vec::<String>::new())?;
    let mut name = DistinguishedName::new();
    name.push(DnType::CommonName, CA_NAME);
    params.distinguished_name = name;
    params.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
    params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    Ok(params)
}

fn set_validity(params: &mut CertificateParams, days: i64) {
    let ymd = |offset: i64| {
        let day = Utc::now() + Duration::days(offset);
        rcgen::date_time_ymd(day.year(), day.month() as u8, day.day() as u8)
    };
    params.not_before = ymd(-1);
    params.not_after = ymd(days);
}

fn write_private(path: &Path, content: &str) -> std::io::Result<()> {
    let mut options = fs::OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)?.write_all(content.as_bytes())
}

pub fn fingerprint(pem: &str) -> anyhow::Result<String> {
    let body: String = pem
        .lines()
        .filter(|line| !line.starts_with("-----"))
        .collect();
    let der = base64::engine::general_purpose::STANDARD
        .decode(body.trim())
        .context("invalid CA PEM")?;
    Ok(format!("{:x}", Sha256::digest(der)))
}

/// 自管模式下公开的 CA 证书；用户自带证书时不提供。
pub fn published_ca(config: &ClientConfig) -> Option<String> {
    if config.tls_cert.is_some() {
        return None;
    }
    fs::read_to_string(managed_dir(config).join("ca.pem")).ok()
}

/// 读取或首次创建自管 CA。私钥只保存在实例状态目录，权限 0600。
fn load_or_create_ca(dir: &Path) -> anyhow::Result<(String, KeyPair)> {
    let (cert_path, key_path) = (dir.join("ca.pem"), dir.join("ca-key.pem"));
    if cert_path.exists() || key_path.exists() {
        let pem = fs::read_to_string(&cert_path)
            .with_context(|| format!("read {}", cert_path.display()))?;
        let key = KeyPair::from_pem(&fs::read_to_string(&key_path)?)
            .with_context(|| format!("parse {}", key_path.display()))?;
        return Ok((pem, key));
    }
    fs::create_dir_all(dir)?;
    let key = KeyPair::generate()?;
    let mut params = ca_params()?;
    set_validity(&mut params, 3650);
    let pem = params.self_signed(&key)?.pem();
    write_private(&key_path, &key.serialize_pem())?;
    write_private(&cert_path, &pem)?;
    tracing::info!(event = "ClientCaCreated", path = %cert_path.display());
    Ok((pem, key))
}

/// 本机网卡地址（含 EasyTier 等虚拟网卡），排除回环与 IPv6 链路本地地址。
fn interface_ips() -> Vec<IpAddr> {
    let mut ips: Vec<IpAddr> = if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .map(|i| i.ip())
        .filter(|ip| match ip {
            IpAddr::V4(v4) => !v4.is_loopback() && !v4.is_link_local(),
            IpAddr::V6(v6) => !v6.is_loopback() && (v6.segments()[0] & 0xffc0) != 0xfe80,
        })
        .collect();
    ips.sort_by_key(|ip| ip.is_ipv6());
    ips.dedup();
    ips
}

fn hosts(config: &ClientConfig) -> Vec<String> {
    if !config.advertise.is_empty() {
        return config.advertise.clone();
    }
    let ips = if config.listen.ip().is_unspecified() {
        interface_ips()
    } else {
        vec![config.listen.ip()]
    };
    ips.into_iter().map(|ip| ip.to_string()).collect()
}

/// 客户端可尝试的地址，按优先级排列：显式 `advertise`，否则监听地址或本机网卡。
pub fn candidate_addresses(config: &ClientConfig) -> Vec<String> {
    let port = config.listen.port();
    hosts(config)
        .into_iter()
        .map(|host| match host.parse::<IpAddr>() {
            Ok(ip) => SocketAddr::new(ip, port).to_string(),
            Err(_) => format!("{host}:{port}"),
        })
        .collect()
}

fn subject_names(config: &ClientConfig) -> Vec<String> {
    let mut names = vec!["localhost".to_string(), "127.0.0.1".to_string()];
    names.extend(hosts(config));
    if !config.advertise.is_empty() && config.listen.ip().is_unspecified() {
        names.extend(interface_ips().into_iter().map(|ip| ip.to_string()));
    }
    names.sort();
    names.dedup();
    names
}

/// 加载服务端 TLS 材料。两个证书路径都配置时沿用用户证书；都省略时启用自管 CA。
pub fn load(config: &ClientConfig) -> anyhow::Result<ServerTls> {
    match (&config.tls_cert, &config.tls_key) {
        (Some(cert), Some(key)) => Ok(ServerTls {
            cert_pem: fs::read_to_string(cert)
                .with_context(|| format!("read {}", cert.display()))?,
            key_pem: fs::read_to_string(key).with_context(|| format!("read {}", key.display()))?,
            ca_fingerprint: None,
        }),
        (None, None) => {
            let (ca_pem, ca_key) = load_or_create_ca(&managed_dir(config))?;
            // 签发者只需要 DN 与公钥；用同一密钥重建即可保持与已发布 CA 一致。
            let issuer = ca_params()?.self_signed(&ca_key)?;
            let names = subject_names(config);
            let mut params = CertificateParams::new(names.clone())?;
            params
                .distinguished_name
                .push(DnType::CommonName, "Harness Client");
            set_validity(&mut params, 397);
            params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
            params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
            let key = KeyPair::generate()?;
            let cert = params.signed_by(&key, &issuer, &ca_key)?;
            tracing::info!(event = "ClientCertificateIssued", subject_names = ?names);
            Ok(ServerTls {
                cert_pem: cert.pem(),
                key_pem: key.serialize_pem(),
                ca_fingerprint: Some(fingerprint(&ca_pem)?),
            })
        }
        _ => anyhow::bail!("[client] tls_cert and tls_key must be set together"),
    }
}

/// 单个配对链接：实例 ID、一次性配对码、CA 指纹与候选地址，客户端扫码或粘贴即可。
pub fn pairing_link(
    instance_id: uuid::Uuid,
    code: &str,
    fingerprint: Option<&str>,
    addresses: &[String],
) -> String {
    let mut link = format!("harness://pair?i={instance_id}&c={code}");
    if let Some(fingerprint) = fingerprint {
        link.push_str(&format!("&f={fingerprint}"));
    }
    for address in addresses {
        link.push_str("&a=");
        for byte in address.bytes() {
            match byte {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'.' | b'-' | b'_' | b':' => {
                    link.push(byte as char)
                }
                _ => link.push_str(&format!("%{byte:02X}")),
            }
        }
    }
    link
}

/// 终端二维码，按正色输出：浅色终端下为白底黑码，静区留白。
/// 反色会把静区画成实心黑边，浅色终端与多数扫码器都无法识别；
/// 深色终端改用 [`write_qr_svg`] 写出的文件扫码。
pub fn terminal_qr(text: &str) -> anyhow::Result<String> {
    let code = qrcode::QrCode::with_error_correction_level(text, qrcode::EcLevel::L)?;
    Ok(code
        .render::<qrcode::render::unicode::Dense1x2>()
        .quiet_zone(true)
        .build())
}

/// 与终端字体、配色无关的二维码文件，供预览后扫码。
pub fn write_qr_svg(config: &ClientConfig, text: &str) -> anyhow::Result<PathBuf> {
    let code = qrcode::QrCode::with_error_correction_level(text, qrcode::EcLevel::M)?;
    let svg = code
        .render::<qrcode::render::svg::Color>()
        .quiet_zone(true)
        .min_dimensions(360, 360)
        .build();
    let path = managed_dir(config).join("pair-qr.svg");
    fs::write(&path, svg).with_context(|| format!("write {}", path.display()))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(dir: &Path) -> ClientConfig {
        ClientConfig {
            listen: "0.0.0.0:9443".parse().unwrap(),
            instance_name: "test".into(),
            tls_cert: None,
            tls_key: None,
            state_path: dir.join("state.json"),
            advertise: vec!["harness.local".into(), "10.126.0.2".into()],
            meow: None,
        }
    }

    #[test]
    fn managed_ca_is_created_once_and_signs_server_certificate() {
        let dir = tempfile::tempdir().unwrap();
        let config = config(dir.path());
        let first = load(&config).unwrap();
        let second = load(&config).unwrap();
        // 每次启动重签服务端证书，但 CA 及其指纹保持稳定，已配对设备无需重新信任。
        assert_eq!(first.ca_fingerprint, second.ca_fingerprint);
        assert_ne!(first.cert_pem, second.cert_pem);
        let ca = published_ca(&config).unwrap();
        assert_eq!(first.ca_fingerprint.unwrap(), fingerprint(&ca).unwrap());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(dir.path().join("ca-key.pem"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o077, 0);
        }
    }

    #[test]
    fn user_certificates_disable_managed_ca() {
        let dir = tempfile::tempdir().unwrap();
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let mut config = config(dir.path());
        config.tls_cert = Some(root.join("tests/fixtures/client-tls/test-cert.pem"));
        config.tls_key = Some(root.join("tests/fixtures/client-tls/test-key.pem"));
        assert!(load(&config).unwrap().ca_fingerprint.is_none());
        assert!(published_ca(&config).is_none());
        assert!(!dir.path().join("ca.pem").exists());
        config.tls_key = None;
        assert!(load(&config).is_err());
    }

    #[test]
    fn pairing_link_carries_everything_needed_to_connect() {
        let dir = tempfile::tempdir().unwrap();
        let addresses = candidate_addresses(&config(dir.path()));
        assert_eq!(addresses, ["harness.local:9443", "10.126.0.2:9443"]);
        let id = uuid::Uuid::nil();
        let link = pairing_link(id, "abc", Some("ff"), &addresses);
        assert_eq!(
            link,
            "harness://pair?i=00000000-0000-0000-0000-000000000000&c=abc&f=ff\
             &a=harness.local:9443&a=10.126.0.2:9443"
        );
        let v6 = pairing_link(id, "abc", None, &["[fd00::1]:9443".into()]);
        assert!(v6.ends_with("&a=%5Bfd00::1%5D:9443"));
        assert!(terminal_qr(&link).unwrap().lines().count() > 10);
    }

    #[test]
    fn terminal_qr_keeps_quiet_zone_blank() {
        let link = pairing_link(
            uuid::Uuid::nil(),
            "abc",
            None,
            &["192.168.1.11:9443".into()],
        );
        let qr = terminal_qr(&link).unwrap();
        // 静区必须是空白；渲染成块字符会得到实心黑框，浅色终端下无法定位。
        let first = qr.lines().next().unwrap();
        assert!(first.trim().is_empty(), "quiet zone: {first:?}");
        assert!(
            qr.lines()
                .any(|line| line.contains('█') || line.contains('▀') || line.contains('▄'))
        );
    }

    #[test]
    fn qr_svg_is_written_next_to_state() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_qr_svg(&config(dir.path()), "harness://pair?i=test").unwrap();
        assert_eq!(path, dir.path().join("pair-qr.svg"));
        let svg = fs::read_to_string(&path).unwrap();
        assert!(svg.contains("<svg"), "unexpected svg output");
    }
}
