use serde_json::Value;

use crate::FormatError;

pub const FIRST_VERSION: u64 = 1;

pub type Migration = fn(Value) -> Result<Value, String>;

pub const MIGRATIONS: &[Migration] = &[];

pub const fn latest_version(steps: &[Migration]) -> u64 {
    FIRST_VERSION + steps.len() as u64
}

pub fn migrate(project: Value, version: u64, steps: &[Migration]) -> Result<Value, FormatError> {
    let supported = latest_version(steps);
    if version < FIRST_VERSION {
        return Err(FormatError::InvalidVersion(version.into()));
    }
    if version > supported {
        return Err(FormatError::NewerVersion { version, supported });
    }
    steps
        .iter()
        .zip(FIRST_VERSION..)
        .filter(|&(_, from)| from >= version)
        .try_fold(project, |project, (step, from)| {
            step(project).map_err(|reason| FormatError::Migration { from, reason })
        })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn mark(project: Value, step: &str) -> Value {
        let mut project = project;
        if let Some(steps) = project["steps"].as_array_mut() {
            steps.push(step.into());
        }
        project
    }

    fn one_to_two(project: Value) -> Result<Value, String> {
        Ok(mark(project, "1→2"))
    }

    fn two_to_three(project: Value) -> Result<Value, String> {
        Ok(mark(project, "2→3"))
    }

    fn refuse(_: Value) -> Result<Value, String> {
        Err("the clips cannot be converted".into())
    }

    const STEPS: &[Migration] = &[one_to_two, two_to_three];

    fn steps_run_from(version: u64) -> Result<Value, FormatError> {
        migrate(json!({ "steps": [] }), version, STEPS)
    }

    #[test]
    fn the_shipped_chain_matches_the_current_version() {
        assert_eq!(latest_version(MIGRATIONS), crate::CURRENT_VERSION);
    }

    #[test]
    fn steps_run_in_order_from_the_source_version() {
        assert_eq!(latest_version(STEPS), 3);
        assert_eq!(
            steps_run_from(1).unwrap(),
            json!({ "steps": ["1→2", "2→3"] })
        );
        assert_eq!(steps_run_from(2).unwrap(), json!({ "steps": ["2→3"] }));
        assert_eq!(steps_run_from(3).unwrap(), json!({ "steps": [] }));
    }

    #[test]
    fn versions_outside_the_chain_are_refused() {
        assert!(matches!(
            steps_run_from(4),
            Err(FormatError::NewerVersion {
                version: 4,
                supported: 3
            })
        ));
        assert!(matches!(
            steps_run_from(0),
            Err(FormatError::InvalidVersion(version)) if version == 0
        ));
    }

    #[test]
    fn a_failing_step_names_its_source_version() {
        let error = migrate(json!({}), 1, &[one_to_two, refuse]).unwrap_err();
        assert!(matches!(
            error,
            FormatError::Migration { from: 2, ref reason } if reason == "the clips cannot be converted"
        ));
    }
}
