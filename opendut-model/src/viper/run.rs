use crate::create_id_type;
use crate::viper::ViperTestId;


#[derive(Clone, Debug)]
pub struct ViperRunDeployment {
    pub run_id: ViperRunId,
    pub test_id: ViperTestId,
}

create_id_type!(ViperRunId);
