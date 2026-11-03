use clap::{Parser, ValueEnum};
use std::time::Duration;
use thingscan_core::Device;
use thingscan_core::reach;
use thingscan_core::scan::{self, Event};

/// Find smart-home devices nearby the way the SmartThings app does:
/// mDNS, SSDP/UPnP, OCF over CoAP, BLE adverts, and Wi-Fi SoftAP setup networks.
#[derive(Parser)]
#[command(version)]
struct Cli {
    /// How long to listen, in seconds.
    #[arg(short, long, default_value_t = 8)]
    timeout: u64,

    /// Channels to use (default: all).
    #[arg(short, long, value_enum, value_delimiter = ',')]
    only: Vec<Channel>,

    /// Print JSON instead of a table.
    #[arg(long)]
    json: bool,

    /// Show every BLE advertiser, not just Samsung / SmartThings / Matter ones.
    #[arg(long)]
    all_ble: bool,

    /// Extra DNS-SD service types to browse, e.g. `_hap._tcp`.
    #[arg(long = "mdns-type")]
    mdns_types: Vec<String>,

    /// Only use multicast for OCF; don't also probe each host on the local subnet.
    #[arg(long)]
    no_ocf_sweep: bool,

    /// Skip the HTTP lookup against Samsung TVs (port 8001).
    #[arg(long)]
    no_enrich: bool,

    /// Show every detail field under each device.
    #[arg(short, long)]
    verbose: bool,

    /// After scanning, check that each device answers again (what the app does on launch).
    #[arg(long)]
    check: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Channel {
    Mdns,
    Ssdp,
    Ocf,
    Ble,
    Wifi,
}

impl From<Channel> for scan::Channel {
    fn from(c: Channel) -> Self {
        match c {
            Channel::Mdns => scan::Channel::Mdns,
            Channel::Ssdp => scan::Channel::Ssdp,
            Channel::Ocf => scan::Channel::Ocf,
            Channel::Ble => scan::Channel::Ble,
            Channel::Wifi => scan::Channel::Wifi,
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let timeout = Duration::from_secs(cli.timeout.max(2));
    let options = scan::Options {
        timeout,
        wifi_timeout: timeout,
        channels: if cli.only.is_empty() {
            scan::Channel::ALL.to_vec()
        } else {
            cli.only.iter().map(|&c| c.into()).collect()
        },
        all_ble: cli.all_ble,
        mdns_types: cli.mdns_types.clone(),
        ocf_sweep: !cli.no_ocf_sweep,
        enrich_tvs: !cli.no_enrich,
    };

    if !cli.json {
        eprintln!("Scanning for {}s…", timeout.as_secs());
    }
    let mut warnings = Vec::new();
    let devices = scan::run(options, |event| {
        if let Event::Done { channel, error: Some(e), .. } = event {
            let hint = if channel == scan::Channel::Wifi && e.contains("didn't finish") {
                "; try `--only wifi -t 20`"
            } else {
                ""
            };
            warnings.push(format!("{}: {e}{hint}", channel.label()));
        }
    })
    .await;

    if cli.check {
        let targets = devices
            .iter()
            .map(|d| reach::Target { id: d.key.clone(), ids: d.ids.clone(), endpoints: d.endpoints.clone() })
            .collect();
        reach::check(targets, None, |report| {
            let name = devices.iter().find(|d| d.key == report.id).map_or("?", |d| d.name.as_str());
            eprintln!("{:?}  {name}  {}", report.status, report.address.as_deref().unwrap_or("-"));
            for p in &report.probes {
                let ms = p.latency_ms.map(|ms| format!(" {ms}ms")).unwrap_or_default();
                eprintln!("    {:<6} {:<40} {:?}{ms}  {}", p.via, p.target, p.status, p.detail);
            }
        })
        .await;
        return Ok(());
    }

    if cli.json {
        let out = serde_json::json!({ "devices": devices, "warnings": warnings });
        println!("{}", serde_json::to_string_pretty(&out)?);
    } else {
        print_table(&devices, cli.verbose);
        for w in &warnings {
            eprintln!("warning: {w}");
        }
    }
    Ok(())
}

fn print_table(devices: &[Device], verbose: bool) {
    if devices.is_empty() {
        println!("No devices found.");
        return;
    }
    let rows: Vec<[String; 5]> = devices
        .iter()
        .map(|d| {
            let sources = d.sources.iter().map(|s| s.to_string()).collect::<Vec<_>>().join("+");
            let maker =
                [d.manufacturer.as_deref(), d.model.as_deref()].into_iter().flatten().collect::<Vec<_>>().join(" ");
            [d.kind.to_string(), d.name.clone(), d.address.clone(), sources, maker]
        })
        .collect();
    let headers = ["KIND", "NAME", "ADDRESS", "VIA", "MAKE / MODEL"];
    let widths: Vec<usize> = (0..headers.len())
        .map(|i| rows.iter().map(|r| r[i].chars().count()).chain([headers[i].len()]).max().unwrap().min(40))
        .collect();
    let line = |cells: &[&str]| {
        cells
            .iter()
            .zip(&widths)
            .map(|(c, w)| format!("{:<w$}", truncate(c, *w), w = *w))
            .collect::<Vec<_>>()
            .join("  ")
            .trim_end()
            .to_string()
    };
    println!("{}", line(&headers));
    for (row, device) in rows.iter().zip(devices) {
        println!("{}", line(&row.each_ref().map(String::as_str)));
        if verbose {
            for (k, v) in &device.details {
                println!("    {k}: {v}");
            }
        }
    }
    println!("\n{} device(s)", devices.len());
}

fn truncate(s: &str, width: usize) -> String {
    if s.chars().count() <= width {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(width.saturating_sub(1)).collect();
        t.push('…');
        t
    }
}
