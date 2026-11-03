//! DNS-SD browsing for the service types the SmartThings app declares in its
//! Info.plist `NSBonjourServices` (iOS refuses to browse anything not listed there).

use crate::device::{Endpoint, Finding, Kind, Sink, Source, hex};
use crate::matter;
use anyhow::Result;
use mdns_sd::{ResolvedService, ServiceDaemon, ServiceEvent};
use std::collections::HashMap;
use std::net::IpAddr;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::Instant;

/// From SmartThings.app/Info.plist. `_lnp._tcp` and `_bonjour._tcp` are also listed
/// there but look like local-network-permission triggers, so they are skipped.
pub const APP_SERVICE_TYPES: &[&str] = &[
    "_smartthings._tcp.local.",
    "_smartthings-hedge._tcp.local.",
    "_matterc._udp.local.",
    "_matterd._udp.local.",
    "_matter._tcp.local.",
    "_meshcop._udp.local.",
];

pub async fn scan(timeout: Duration, extra_types: &[String], sink: &Sink) -> Result<()> {
    let mut types: Vec<String> = APP_SERVICE_TYPES.iter().map(|s| s.to_string()).collect();
    types.extend(extra_types.iter().map(|t| normalize_type(t)));
    browse(types, timeout, sink).await
}

/// Browse `types` for `wait` and return the address each resolved instance answered on,
/// keyed by its full name.
pub async fn resolve(types: &[String], wait: Duration) -> Result<HashMap<String, String>> {
    let (tx, mut rx) = mpsc::unbounded_channel();
    browse(types.to_vec(), wait, &tx).await?;
    drop(tx);
    let mut out = HashMap::new();
    while let Some(f) = rx.recv().await {
        if let Some(name) = f.details.get("fullname") {
            out.insert(name.clone(), f.address);
        }
    }
    Ok(out)
}

async fn browse(types: Vec<String>, timeout: Duration, sink: &Sink) -> Result<()> {
    let daemon = ServiceDaemon::new()?;
    let deadline = Instant::now() + timeout;
    let mut tasks = Vec::new();
    for ty in types {
        let rx = daemon.browse(&ty)?;
        let sink = sink.clone();
        tasks.push(tokio::spawn(async move {
            // A service resolves again on every interface; the caller keeps the latest.
            while let Ok(Ok(event)) = tokio::time::timeout_at(deadline, rx.recv_async()).await {
                if let ServiceEvent::ServiceResolved(svc) = event {
                    let _ = sink.send(to_finding(&svc));
                }
            }
        }));
    }
    for task in tasks {
        task.await?;
    }
    let _ = daemon.shutdown();
    Ok(())
}

fn normalize_type(t: &str) -> String {
    let t = t.trim_end_matches('.');
    let t = t.strip_suffix(".local").unwrap_or(t);
    format!("{t}.local.")
}

fn to_finding(svc: &ResolvedService) -> Finding {
    let instance = svc.fullname.strip_suffix(&svc.ty_domain).unwrap_or(&svc.fullname).trim_end_matches('.').to_string();
    let ip = pick_ip(svc);
    let address = match ip {
        Some(IpAddr::V6(v6)) => format!("[{v6}]:{}", svc.port),
        Some(v4) => format!("{v4}:{}", svc.port),
        None => format!("{}:{}", svc.host.trim_end_matches('.'), svc.port),
    };
    let txt = |k: &str| svc.txt_properties.get_property_val_str(k).map(str::to_string);
    let ty = svc.ty_domain.trim_end_matches(".local.");

    let mut f = match ty {
        "_matterc._udp" => {
            let cm = txt("CM").unwrap_or_default();
            let kind = if cm == "0" { Kind::Matter } else { Kind::ReadyToAdd };
            let dt = txt("DT").and_then(|d| d.parse::<u32>().ok());
            let name = txt("DN")
                .or_else(|| dt.and_then(matter::device_type_name).map(str::to_string))
                .unwrap_or_else(|| format!("Matter device {instance}"));
            let mut f = Finding::new(Source::Mdns, kind, name, address)
                .detail("matter.discriminator", txt("D").unwrap_or_default())
                .detail("matter.commissioning", matter::commissioning_mode(&cm));
            if let Some(dt) = dt {
                let label = matter::device_type_name(dt).unwrap_or("unknown");
                f = f.detail("matter.device_type", format!("0x{dt:04X} {label}"));
            }
            if let Some((vid, pid)) = txt("VP").as_deref().and_then(|vp| vp.split_once('+')) {
                f = f.detail("matter.vendor_id", vid).detail("matter.product_id", pid);
            }
            f
        }
        "_matter._tcp" => {
            // Operational instance name is <compressed fabric id>-<node id>.
            let mut f = Finding::new(Source::Mdns, Kind::Matter, format!("Matter node {instance}"), address);
            if let Some((fabric, node)) = instance.split_once('-') {
                f = f.detail("matter.fabric", fabric).detail("matter.node", node);
            }
            f
        }
        "_matterd._udp" => Finding::new(Source::Mdns, Kind::Hub, format!("Matter commissioner {instance}"), address),
        "_meshcop._udp" => {
            let name = match (txt("vn"), txt("mn")) {
                (Some(vn), Some(mn)) => format!("{vn} {mn}"),
                _ => instance.clone(),
            };
            let xp = svc.txt_properties.get("xp").and_then(|p| p.val()).map(hex);
            Finding::new(Source::Mdns, Kind::ThreadBorderRouter, name, address)
                .detail("thread.network", txt("nn").unwrap_or_default())
                .detail("thread.ext_pan_id", xp.unwrap_or_default())
                .detail("thread.version", txt("tv").unwrap_or_default())
        }
        "_smartthings._tcp" | "_smartthings-hedge._tcp" => {
            Finding::new(Source::Mdns, Kind::Hub, instance.clone(), address)
        }
        _ => Finding::new(Source::Mdns, Kind::Other, instance.clone(), address),
    };

    // Keep raw TXT for the SmartThings-specific and user-requested types, whose keys we don't know.
    if matches!(f.kind, Kind::Hub | Kind::Other) {
        for p in svc.txt_properties.iter() {
            f.details.insert(format!("txt.{}", p.key()), p.val_str().to_string());
        }
    }
    f = f
        .id(format!("mdns:{}", svc.fullname))
        .endpoint(Endpoint::Mdns { service: svc.ty_domain.clone(), fullname: svc.fullname.clone() });
    f.ip = ip;
    f.details.insert("service".into(), ty.to_string());
    f.details.insert("fullname".into(), svc.fullname.clone());
    f.details.insert("host".into(), svc.host.trim_end_matches('.').to_string());
    f
}

/// Prefer a routable IPv4 address so findings merge with SSDP/OCF results by IP.
fn pick_ip(svc: &ResolvedService) -> Option<IpAddr> {
    let mut ips: Vec<IpAddr> = svc.addresses.iter().map(|a| a.to_ip_addr()).collect();
    ips.sort_by_key(|ip| match ip {
        _ if ip.is_loopback() => 3,
        IpAddr::V4(_) => 0,
        IpAddr::V6(v6) if !v6.is_unicast_link_local() => 1,
        IpAddr::V6(_) => 2,
    });
    ips.into_iter().next()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_service_types() {
        assert_eq!(normalize_type("_hap._tcp"), "_hap._tcp.local.");
        assert_eq!(normalize_type("_hap._tcp.local."), "_hap._tcp.local.");
        assert_eq!(normalize_type("_hap._tcp.local"), "_hap._tcp.local.");
    }
}
