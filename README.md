# ping-exporter

A ping exporter for Prometheus.

Reports the following metrics (per-IP, per-netns):

- `total_pings`
- `successful_pings`
- `successful_ping_wait_sum`

```
Usage: ping-exporter [OPTIONS] [TARGET]...

Arguments:
  [TARGET]...  Target IPs

Options:
  -l, --listen <LISTEN>        Listen address (e.g. 127.0.0.1:3000)
  -c, --config <CONFIG>        Config path
  -I, --interface <INTERFACE>  Default ping interface (interface name or IP to bind to)
  -n, --netns <NETNS>          Default network namespace name
  -i, --interval <INTERVAL>    Default ping interval (in seconds)
  -t, --timeout <TIMEOUT>      Default ping timeout (in seconds)
      --type <TYPE>            Default ICMP socket type [possible values: dgram, raw]
      --ttl <TTL>              Default ICMP TTL
  -h, --help                   Print help
```

TOML config sample (all fields are optional):

```
listen = "127.0.0.1:3000"
type = "dgram"
interface = "eth0"
interval = 5
timeout = 10
# interface = "192.168.1.1"
netns = "test"
ttl = 128
targets = [ "8.8.8.8", { target = "8.8.4.4", netns = null, interval = 5, timeout = 1 } ]
```
