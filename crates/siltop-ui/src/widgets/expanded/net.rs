//! Expanded network: download and upload cells.

use siltop_core::net_rate;

use super::cells::graph_spec;
use super::{ID_NET_DOWN, ID_NET_UP};
use crate::widgets::AppView;
use crate::widgets::grid::Band;

pub(super) fn net_bands(view: &AppView<'_>) -> Vec<Band> {
    vec![Band {
        take_leftover: true,
        ..Band::new(
            5,
            vec![
                graph_spec(
                    ID_NET_DOWN,
                    "down",
                    Some(net_rate(view.snapshot.network.rx_bps, view.net_unit)),
                    true,
                ),
                graph_spec(
                    ID_NET_UP,
                    "up",
                    Some(net_rate(view.snapshot.network.tx_bps, view.net_unit)),
                    true,
                ),
            ],
        )
    }]
}
