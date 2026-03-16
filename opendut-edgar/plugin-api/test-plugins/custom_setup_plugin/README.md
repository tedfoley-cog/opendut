# Custom Setup Plugin

A template/example EDGAR setup plugin that demonstrates how to build custom device setup automation using the openDuT plugin API.

## What It Does

This plugin serves as a starter template for creating custom EDGAR setup plugins. Out of the box, it:

- Reports itself as unfulfilled (so the `execute` step always runs)
- Runs a simple shell command via the host `call_command` function to demonstrate host interaction
- Logs the command output and returns a success message

## Building

```sh
cargo component build --target wasm32-wasip2 --release
```

Or use the shared build script from the `test-plugins/` directory:

```sh
./build-distribution.sh
```

## Deployment

1. Build the plugin as described above.
2. Copy the resulting `.wasm` file (e.g., `custom_setup_plugin.wasm`) into the `plugins/` directory of your EDGAR installation.
3. Reference the plugin in `plugins.txt` within the `plugins/` directory:
   ```
   custom_setup_plugin.wasm
   ```

## Customization

To adapt this plugin for real device setup scenarios:

- **`check_fulfilled()`**: Modify this to inspect actual device state (e.g., check if a configuration file exists, verify a service is running, or query hardware status). Return `TaskFulfilled::Yes` when setup is already complete to skip redundant execution.
- **`execute()`**: Replace the example command with your actual setup logic. Use `call_command` to run shell commands on the host for tasks like installing packages, configuring network interfaces, flashing firmware, or starting services.
- **`description()`**: Update the description to reflect what your plugin does.

### Example: Checking for a Configuration File

```rust
fn check_fulfilled() -> Result<TaskFulfilled, ()> {
    let result = call_command("test", &vec![String::from("-f"), String::from("/etc/my-device.conf")]);
    match result {
        Ok(_) => Ok(TaskFulfilled::Yes),
        Err(_) => Ok(TaskFulfilled::No),
    }
}
```

### Example: Running a Setup Script

```rust
fn execute() -> Result<Success, ()> {
    let result = call_command("/opt/setup/configure-device.sh", &vec![]);
    match result {
        Ok(output) => {
            info(format!("Setup output: {}", output).as_str());
            Ok(Success::message("Device configured successfully"))
        }
        Err(_) => Err(()),
    }
}
```
