use opendut_edgar_plugin_api::plugin::{export, info};
use opendut_edgar_plugin_api::plugin::host::call_command;
use opendut_edgar_plugin_api::plugin::task::{Guest, Success, TaskFulfilled};

struct CustomSetupPlugin;

impl Guest for CustomSetupPlugin {
    async fn description() -> String {
        String::from("Custom Setup Plugin - Demonstrates device setup automation")
    }

    fn check_fulfilled() -> Result<TaskFulfilled, ()> {
        Ok(TaskFulfilled::No)
    }

    fn execute() -> Result<Success, ()> {
        let command_result = call_command("echo", &vec![String::from("Custom setup plugin executed successfully")]);

        match command_result {
            Ok(output) => {
                info(format!("Command output: {}", output).as_str());
                Ok(Success::message("Custom setup completed successfully"))
            }
            Err(_) => {
                Err(())
            }
        }
    }
}

export!(CustomSetupPlugin with_types_in opendut_edgar_plugin_api::plugin::bindings);
