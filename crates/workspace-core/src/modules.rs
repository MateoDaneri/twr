use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ModuleSpec {
    pub id: String,
    pub dependencies: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ModuleState {
    Cold,
    Loading,
    Ready,
    Failed(String),
}

#[derive(Debug)]
pub struct Modules {
    specs: BTreeMap<String, ModuleSpec>,
    states: BTreeMap<String, ModuleState>,
}

impl Modules {
    pub fn new(specs: Vec<ModuleSpec>) -> Result<Self, String> {
        if specs.len() > 64 {
            return Err("module limit exceeded".into());
        }
        let mut result = Self {
            specs: BTreeMap::new(),
            states: BTreeMap::new(),
        };
        for spec in specs {
            if spec.id.is_empty() || result.specs.contains_key(&spec.id) {
                return Err("duplicate or empty module".into());
            }
            result.states.insert(spec.id.clone(), ModuleState::Cold);
            result.specs.insert(spec.id.clone(), spec);
        }
        for id in result.specs.keys() {
            result.order(
                id,
                &mut BTreeSet::new(),
                &mut BTreeSet::new(),
                &mut Vec::new(),
            )?;
        }
        Ok(result)
    }
    fn order(
        &self,
        id: &str,
        visiting: &mut BTreeSet<String>,
        visited: &mut BTreeSet<String>,
        out: &mut Vec<String>,
    ) -> Result<(), String> {
        if visited.contains(id) {
            return Ok(());
        }
        if !visiting.insert(id.into()) {
            return Err("module dependency cycle".into());
        }
        let spec = self
            .specs
            .get(id)
            .ok_or_else(|| format!("unknown module {id}"))?;
        for dependency in &spec.dependencies {
            self.order(dependency, visiting, visited, out)?;
        }
        visiting.remove(id);
        visited.insert(id.into());
        out.push(id.into());
        Ok(())
    }
    /// Returns dependency order. Load on a worker, then call complete in that order.
    pub fn request(&mut self, id: &str) -> Result<Vec<String>, String> {
        let mut order = Vec::new();
        self.order(id, &mut BTreeSet::new(), &mut BTreeSet::new(), &mut order)?;
        if order
            .iter()
            .any(|id| matches!(self.states[id], ModuleState::Failed(_)))
        {
            return Err("failed module needs an explicit retry".into());
        }
        order.retain(|id| self.states[id] == ModuleState::Cold);
        for id in &order {
            self.states.insert(id.clone(), ModuleState::Loading);
        }
        Ok(order)
    }
    pub fn complete(&mut self, id: &str, result: Result<(), String>) -> Result<(), String> {
        if self.states.get(id) != Some(&ModuleState::Loading) {
            return Err("module is not loading".into());
        }
        if result.is_ok()
            && self.specs[id]
                .dependencies
                .iter()
                .any(|id| self.states[id] != ModuleState::Ready)
        {
            return Err("dependencies are not ready".into());
        }
        self.states.insert(
            id.into(),
            match result {
                Ok(()) => ModuleState::Ready,
                Err(e) => ModuleState::Failed(e),
            },
        );
        Ok(())
    }
    pub fn state(&self, id: &str) -> Option<&ModuleState> {
        self.states.get(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn opening_tree_loads_only_its_dependency_chain_once() {
        let specs = vec![
            ModuleSpec {
                id: "filesystem".into(),
                dependencies: vec![],
            },
            ModuleSpec {
                id: "tree".into(),
                dependencies: vec!["filesystem".into()],
            },
            ModuleSpec {
                id: "agents".into(),
                dependencies: vec![],
            },
        ];
        let mut modules = Modules::new(specs).unwrap();
        assert_eq!(modules.request("tree").unwrap(), vec!["filesystem", "tree"]);
        assert!(modules.complete("tree", Ok(())).is_err());
        modules.complete("filesystem", Ok(())).unwrap();
        modules.complete("tree", Ok(())).unwrap();
        assert!(modules.request("tree").unwrap().is_empty());
        assert_eq!(modules.state("agents"), Some(&ModuleState::Cold));
    }
    #[test]
    fn missing_dependencies_and_cycles_fail_before_loading() {
        let cycle = vec![
            ModuleSpec {
                id: "a".into(),
                dependencies: vec!["b".into()],
            },
            ModuleSpec {
                id: "b".into(),
                dependencies: vec!["a".into()],
            },
        ];
        assert!(Modules::new(cycle).is_err());
        assert!(
            Modules::new(vec![ModuleSpec {
                id: "a".into(),
                dependencies: vec!["missing".into()]
            }])
            .is_err()
        );
    }
}
