#[derive(Debug, Clone, PartialEq, Default)]
pub struct NetworkSnapshot {
    /// Headline interface: the primary (default-route) interface, or the
    /// busiest listed physical one when there is no default route.
    pub iface: String,
    /// Headline interface, bits per second.
    pub rx_bps: u64,
    /// Headline interface, bits per second.
    pub tx_bps: u64,
    /// BSD name of the interface carrying the default route, if any.
    pub primary: Option<String>,
    /// Physical interfaces that are up and in use, plus the primary if it
    /// is virtual (a VPN `utunN`).
    pub ifaces: Vec<IfaceRate>,
}

/// One interface's exact 64-bit counters and rates (bits per second).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct IfaceRate {
    pub name: String,
    pub rx_bps: u64,
    pub tx_bps: u64,
    /// Lifetime byte counters from `ifmib` (not truncated to 32 bits).
    pub rx_bytes: u64,
    pub tx_bytes: u64,
    /// Link speed in bits per second; `None` when the driver reports 0 or
    /// the 0xFFFFFFFF "unknown" sentinel.
    pub link_bps: Option<u64>,
    pub primary: bool,
}
