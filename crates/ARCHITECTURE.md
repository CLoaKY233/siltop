# Crate map

| Crate | Role | May depend on |
| --- | --- | --- |
| `siltop-core` | Errors, snapshot types, history + series registry (`series.rs`: every recorded series and its push rules), config, surfaces | nothing in this workspace |
| `siltop-metrics` | Sampling. No ratatui. | core |
| `siltop-ui` | Theme, braille, layout, widgets | core, ratatui |
| `siltop` | Binary: event loop, TEA | core, metrics, ui |

`siltop-metrics` must never import `ratatui` or `siltop-ui`.
`siltop-ui` must never import `siltop-metrics`.

Vertical slice: Work + Glance chrome, Mach CPU, memory, processes, filter/kill, braille history.

## FFI boundary (metrics)

`siltop-metrics/src/iokit.rs` is the only place that declares `IOKit` or
`CoreFoundation` symbols. Collectors hold owned handles in its RAII types:
`IoObject` (`IOObjectRelease` on drop), `IoConnect` (`IOServiceClose`) and
`CfOwned` (`CFRelease`). Borrowed values are `CfRef<'a>`, tied to the owner's
lifetime, so a Get-rule value can't outlive its dictionary. Property and
dictionary keys are `CfKey`s created once per collector, never per lookup.
Read single registry properties (`iokit::property`) rather than copying a
whole property table: `PerformanceStatistics` costs 0.012 ms against 0.71 ms.

Every `unsafe` block carries a `// SAFETY:` comment; clippy's
`undocumented_unsafe_blocks` is denied workspace-wide.
