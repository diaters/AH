pub mod approval;
pub mod collection;
pub mod consolidation;
pub mod experience_hook;
pub mod governance;
pub mod profile_generation;
pub mod profile_update;
pub mod writeback;

pub(crate) use approval::experience_approval_result_system;
pub(crate) use collection::{
    experience_collection_completion_system, experience_collection_workitem_system,
    task_terminated_experience_trigger_system,
};
pub(crate) use consolidation::experience_consolidation_trigger_system;
pub(crate) use experience_hook::on_experience_hook_system;
pub(crate) use governance::experience_governance_system;
#[allow(unused_imports)] // 任务 11 系统注册时使用
pub(crate) use profile_generation::{
    profile_generation_completion_system, profile_generation_workitem_system,
};
#[allow(unused_imports)] // 任务 11 系统注册时使用
pub(crate) use profile_update::{
    profile_update_trigger_system, profile_update_writeback_system,
};
pub(crate) use writeback::experience_writeback_system;
