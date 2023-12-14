use std::{
    collections::HashMap,
    convert::Infallible,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    ops::AddAssign,
    path::PathBuf,
    str::FromStr,
    sync::Arc,
    time::Duration,
};

use axum::response::IntoResponse;
use clap::{Parser, ValueEnum};
use dashmap::DashMap;
use serde::{de::Visitor, Deserialize};
use socket2::Type;
use surge_ping::{Client, Pinger, ICMP};
use tokio::{net::TcpListener, sync::mpsc, time::Instant};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Interface {
    Addr(SocketAddr),
    Name(String),
}

impl FromStr for Interface {
    type Err = Infallible;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(if let Ok(val) = s.parse() {
            Interface::Addr(val)
        } else {
            Interface::Name(s.to_owned())
        })
    }
}

struct InterfaceVisitor;

impl<'de> Visitor<'de> for InterfaceVisitor {
    type Value = Interface;
    fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
        formatter.write_str("interface or address to bind to")
    }
    fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        self.visit_borrowed_str(v)
    }
    fn visit_string<E>(self, v: String) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(if let Ok(val) = v.parse() {
            Interface::Addr(val)
        } else {
            Interface::Name(v)
        })
    }
    fn visit_borrowed_str<E>(self, v: &'de str) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(if let Ok(val) = v.parse() {
            Interface::Addr(val)
        } else {
            Interface::Name(v.to_owned())
        })
    }
}

impl<'de> Deserialize<'de> for Interface {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_string(InterfaceVisitor)
    }
}

#[derive(Clone, Debug)]
struct Options {
    interface: Option<Interface>,
    netns: Option<Option<String>>,
    target: IpAddr,
    ttl: Option<u32>,
    timeout: Option<Duration>,
    interval: Option<Duration>,
}

impl FromStr for Options {
    type Err = std::net::AddrParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Options {
            target: s.parse()?,
            netns: None,
            timeout: None,
            interval: None,
            interface: None,
            ttl: None,
        })
    }
}

struct OptionsVisitor;

impl<'de> Visitor<'de> for OptionsVisitor {
    type Value = Options;
    fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
        formatter.write_str("ping option")
    }
    fn visit_borrowed_str<E>(self, v: &'de str) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(Options {
            target: v.parse().map_err(serde::de::Error::custom)?,
            netns: None,
            timeout: None,
            interval: None,
            interface: None,
            ttl: None,
        })
    }
    fn visit_string<E>(self, v: String) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        self.visit_borrowed_str(&v)
    }
    fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        self.visit_borrowed_str(v)
    }
    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: serde::de::MapAccess<'de>,
    {
        let mut ret = Options {
            target: IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            netns: None,
            timeout: None,
            interval: None,
            interface: None,
            ttl: None,
        };
        let mut valid = false;
        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "target" => {
                    ret.target = map.next_value()?;
                    valid = true;
                }
                "interface" => ret.target = map.next_value()?,
                "ttl" => ret.ttl = map.next_value()?,
                "timeout" => ret.timeout = map.next_value()?,
                "interval" => ret.interval = map.next_value()?,
                "netns" => ret.netns = Some(map.next_value()?),
                field => {
                    return Err(serde::de::Error::unknown_field(
                        field,
                        &["target", "interface", "ttl", "timeout", "interval", "netns"],
                    ))
                }
            }
        }
        if valid {
            Ok(ret)
        } else {
            Err(serde::de::Error::missing_field("target"))
        }
    }
}

impl<'de> Deserialize<'de> for Options {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_any(OptionsVisitor)
    }
}

#[derive(Copy, Clone, Debug, Default, Deserialize, PartialEq, Eq, Hash, ValueEnum)]
#[serde(rename_all = "lowercase")]
enum SockType {
    #[default]
    Dgram,
    Raw,
}

impl From<SockType> for Type {
    fn from(value: SockType) -> Self {
        match value {
            SockType::Dgram => Self::DGRAM,
            SockType::Raw => Self::RAW,
        }
    }
}

#[derive(Debug, Default, Deserialize)]
struct Config {
    listen: Option<SocketAddr>,
    r#type: Option<SockType>,
    interface: Option<Interface>,
    netns: Option<String>,
    interval: Option<f64>,
    timeout: Option<f64>,
    targets: Vec<Options>,
    ttl: Option<u32>,
}

#[derive(Parser)]
struct Args {
    /// Listen address (e.g. 127.0.0.1:3000)
    #[clap(long, short = 'l')]
    listen: Option<SocketAddr>,
    /// Config path
    #[clap(long, short)]
    config: Option<PathBuf>,
    /// Default ping interface (interface name or IP to bind to)
    #[clap(long, short = 'I')]
    interface: Option<Interface>,
    /// Default network namespace name
    #[clap(long, short)]
    netns: Option<String>,
    /// Default ping interval (in seconds)
    #[clap(long, short)]
    interval: Option<f64>,
    /// Default ping timeout (in seconds)
    #[clap(long, short)]
    timeout: Option<f64>,
    /// Default ICMP socket type
    #[clap(long)]
    r#type: Option<SockType>,
    /// Default ICMP TTL
    #[clap(long)]
    ttl: Option<u32>,
    /// Target IPs
    target: Vec<Options>,
}

#[tokio::main]
async fn main() {
    env_logger::init();
    let args = Args::parse();
    let config = if let Some(config) = args.config {
        toml::from_str(
            &tokio::fs::read_to_string(config)
                .await
                .unwrap_or_else(|err| panic!("{err}")),
        )
        .unwrap_or_else(|err| panic!("{err}"))
    } else {
        Config::default()
    };

    #[derive(Clone, Debug, PartialEq, Eq, Hash)]
    struct CfgOptions {
        iface: Option<Interface>,
        ttl: Option<u32>,
        r#type: SockType,
        netns: Option<String>,
        v6: bool,
    }

    let mut clients = HashMap::<CfgOptions, Arc<Client>>::new();

    #[derive(Copy, Clone, Default)]
    struct Metrics {
        total_pings: u64,
        successful_pings: u64,
        total_successful_ping_duration: f64,
    }

    impl AddAssign for Metrics {
        fn add_assign(&mut self, rhs: Self) {
            self.total_pings += rhs.total_pings;
            self.successful_pings += rhs.successful_pings;
            self.total_successful_ping_duration += rhs.total_successful_ping_duration;
        }
    }

    let metrics = Arc::new(DashMap::<(IpAddr, Option<String>), Metrics>::new());

    for (
        mut id,
        Options {
            interface,
            target,
            ttl,
            timeout,
            interval,
            netns,
        },
    ) in config
        .targets
        .into_iter()
        .chain(args.target.into_iter())
        .enumerate()
    {
        let interface = interface
            .clone()
            .or_else(|| config.interface.clone())
            .or_else(|| args.interface.clone());
        let ttl = ttl.or(config.ttl).or(args.ttl);
        let r#type = config.r#type.or(args.r#type).unwrap_or_default();
        let netns = netns.unwrap_or_else(|| config.netns.clone().or_else(|| args.netns.clone()));
        let interval = interval
            .or_else(|| config.interval.map(Duration::from_secs_f64))
            .or_else(|| args.interval.map(Duration::from_secs_f64));
        let timeout = timeout
            .or_else(|| config.timeout.map(Duration::from_secs_f64))
            .or_else(|| args.timeout.map(Duration::from_secs_f64));
        let client = clients
            .entry(CfgOptions {
                iface: interface.clone(),
                netns: netns.clone(),
                ttl,
                r#type,
                v6: target.is_ipv6(),
            })
            .or_insert_with(|| {
                let mut cfg = surge_ping::Config::builder()
                    .sock_type_hint(r#type.into())
                    .kind(if target.is_ipv6() { ICMP::V6 } else { ICMP::V4 });
                if let Some(ttl) = ttl {
                    cfg = cfg.ttl(ttl);
                }
                if let Some(iface) = interface {
                    cfg = match iface {
                        Interface::Addr(addr) => cfg.bind(addr),
                        Interface::Name(name) => cfg.interface(&name),
                    };
                }
                let cfg = cfg.build();
                let old_netns = netns.clone().map(|netns| {
                    let src =
                        netns_rs::get_from_current_thread().unwrap_or_else(|err| panic!("{err}"));
                    netns_rs::NetNs::get(netns)
                        .unwrap_or_else(|err| panic!("{err}"))
                        .enter()
                        .unwrap_or_else(|err| panic!("{err}"));
                    src
                });
                let client = Arc::new(Client::new(&cfg).unwrap_or_else(|err| panic!("{err}")));
                if let Some(src) = old_netns {
                    src.enter().unwrap_or_else(|err| panic!("{err}"));
                }
                client
            })
            .clone();
        let metrics = metrics.clone();
        tokio::spawn(async move {
            let (tx, mut rx) = mpsc::unbounded_channel::<(Pinger, u16)>();
            let mut now = Instant::now();
            let interval = interval.unwrap_or_else(|| Duration::from_secs(1));
            loop {
                now += interval;
                let (mut pinger, mut id) = if let Ok(x) = rx.try_recv() {
                    x
                } else {
                    let mut pinger = client.pinger(target, (id as u16).into()).await;
                    if let Some(timeout) = timeout {
                        pinger.timeout(timeout);
                    }
                    id += 1;
                    (pinger, 0u16)
                };
                let tx = tx.clone();
                let metrics = metrics.clone();
                let netns = netns.clone();
                tokio::spawn(async move {
                    let mut cur = Metrics {
                        total_pings: 1,
                        successful_pings: 0,
                        total_successful_ping_duration: 0.,
                    };
                    match pinger.ping(id.into(), b"").await {
                        Ok((_pkt, dur)) => {
                            cur.successful_pings += 1;
                            cur.total_successful_ping_duration += dur.as_secs_f64();
                        }
                        Err(err) => log::error!("Ping error: {err}"),
                    }
                    *metrics
                        .entry((target, netns.clone()))
                        .or_default()
                        .value_mut() += cur;
                    id += 1;
                    let _ = tx.send((pinger, id));
                });
                tokio::time::sleep_until(now).await;
            }
        });
    }

    let app = axum::Router::new().route(
        "/metrics",
        axum::routing::get(|| async move {
            let mut s = "".to_owned();
            for info in metrics.iter() {
                let key = info.key();
                let val = *info.value();
                let (ip, netns) = &key;
                let netns = netns.as_deref().unwrap_or_default();
                s.push_str(&format!(
                    "total_pings{{ip=\"{ip}\",netns=\"{netns}\"}} {}\n",
                    val.total_pings
                ));
                s.push_str(&format!(
                    "successful_pings{{ip=\"{ip}\",netns=\"{netns}\"}} {}\n",
                    val.successful_pings
                ));
                s.push_str(&format!(
                    "successful_ping_wait_sum{{ip=\"{ip}\",netns=\"{netns}\"}} {}\n\n",
                    val.total_successful_ping_duration
                ));
            }
            (
                [(
                    axum::http::header::CONTENT_TYPE,
                    axum::http::HeaderValue::from_static("text/plain"),
                )],
                s,
            )
                .into_response()
        }),
    );
    axum::serve::serve(
        TcpListener::bind(config.listen.unwrap_or_else(|| {
            args.listen
                .expect("Please provide the listen address in config or cli arguments")
        }))
        .await
        .unwrap_or_else(|err| panic!("Listen failed:\n{err}")),
        app.into_make_service(),
    )
    .await
    .unwrap_or_else(|err| panic!("Server error:\n{err}"));
}
