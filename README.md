## Usage

Enable LMU's **Settings > Gameplay > Enable Plugins** option and restart the game before connecting.

```rust,no_run
use lmu_shared_memory::{LmuTelemetry, TelemetryUpdate};

let mut telemetry = LmuTelemetry::connect()?;
match telemetry.read()? {
    TelemetryUpdate::Snapshot(snapshot) => {
        println!("throttle: {:.0}%", snapshot.pedals.throttle * 100.0);
    }
    TelemetryUpdate::NoPlayer | TelemetryUpdate::SourceAlive | TelemetryUpdate::Unchanged => {}
}
# Ok::<(), lmu_shared_memory::Error>(())
```

## Compatibility

The packed offsets are based on LMU's `Support\SharedMemoryInterface` layout.

## License

Licensed under the MIT license.
