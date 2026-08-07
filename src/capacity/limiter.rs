//! Declared-capacity budget checking. Operators state a machine's resource
//! budget in `services.json`; this module checks a candidate service's
//! request against that budget plus whatever its siblings on the same
//! machine are currently using. No runtime host introspection — the budget
//! is exactly what's declared, so behavior is identical across bare metal,
//! Docker (cgroup limits notwithstanding), and serverless deployments.

use crate::config::schema::ResourceSpec;

/// Whether `want` fits within `budget` given `used` is already committed, on
/// every dimension. A `None` budget field means that dimension is completely
/// unconstrained.
pub fn fits_within_budget(budget: &ResourceSpec, used: &ResourceSpec, want: &ResourceSpec) -> bool {
    fits(budget.memory_mb, used.memory_mb, want.memory_mb)
        && fits(budget.cpu_cores, used.cpu_cores, want.cpu_cores)
        && fits(budget.gpu_vram_mb, used.gpu_vram_mb, want.gpu_vram_mb)
}

fn fits(budget: Option<u64>, used: Option<u64>, want: Option<u64>) -> bool {
    match budget {
        None => true,
        Some(budget) => used.unwrap_or(0) + want.unwrap_or(0) <= budget,
    }
}

/// Sums resource requests across every currently-occupying sibling on a machine.
pub fn sum_resource_usage<'a>(specs: impl Iterator<Item = &'a ResourceSpec>) -> ResourceSpec {
    specs.fold(ResourceSpec::default(), |mut acc, s| {
        acc.memory_mb = Some(acc.memory_mb.unwrap_or(0) + s.memory_mb.unwrap_or(0));
        acc.cpu_cores = Some(acc.cpu_cores.unwrap_or(0) + s.cpu_cores.unwrap_or(0));
        acc.gpu_vram_mb = Some(acc.gpu_vram_mb.unwrap_or(0) + s.gpu_vram_mb.unwrap_or(0));
        acc
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(
        memory_mb: Option<u64>,
        cpu_cores: Option<u64>,
        gpu_vram_mb: Option<u64>,
    ) -> ResourceSpec {
        ResourceSpec {
            memory_mb,
            cpu_cores,
            gpu_vram_mb,
        }
    }

    #[test]
    fn fits_within_budget_true_when_under_every_dimension() {
        let budget = spec(Some(65536), Some(16), Some(48000));
        let used = spec(Some(16384), Some(4), Some(24000));
        let want = spec(Some(16384), Some(4), Some(24000));
        assert!(fits_within_budget(&budget, &used, &want));
    }

    #[test]
    fn fits_within_budget_false_when_memory_exceeded() {
        let budget = spec(Some(1000), None, None);
        let used = spec(Some(600), None, None);
        let want = spec(Some(500), None, None);
        assert!(!fits_within_budget(&budget, &used, &want));
    }

    #[test]
    fn fits_within_budget_false_when_cpu_exceeded() {
        let budget = spec(None, Some(8), None);
        let used = spec(None, Some(6), None);
        let want = spec(None, Some(4), None);
        assert!(!fits_within_budget(&budget, &used, &want));
    }

    #[test]
    fn fits_within_budget_false_when_gpu_vram_exceeded() {
        let budget = spec(None, None, Some(24000));
        let used = spec(None, None, Some(20000));
        let want = spec(None, None, Some(8000));
        assert!(!fits_within_budget(&budget, &used, &want));
    }

    #[test]
    fn fits_within_budget_exact_boundary_fits() {
        let budget = spec(Some(1000), None, None);
        let used = spec(Some(600), None, None);
        let want = spec(Some(400), None, None);
        assert!(fits_within_budget(&budget, &used, &want));
    }

    #[test]
    fn fits_within_budget_unconstrained_dimension_always_fits() {
        let budget = spec(None, None, None);
        let used = spec(Some(999_999), Some(999), Some(999_999));
        let want = spec(Some(999_999), Some(999), Some(999_999));
        assert!(fits_within_budget(&budget, &used, &want));
    }

    #[test]
    fn fits_within_budget_single_dimension_request_ignores_other_budget_lines() {
        // Service only declares memory_mb; a tight cpu_cores/gpu_vram_mb budget
        // elsewhere must not block it, since it requests zero of those.
        let budget = spec(Some(65536), Some(1), Some(1));
        let used = spec(Some(0), Some(1), Some(1)); // siblings already fully using cpu/gpu
        let want = spec(Some(16384), None, None);
        assert!(fits_within_budget(&budget, &used, &want));
    }

    #[test]
    fn sum_resource_usage_adds_across_multiple_specs() {
        let specs = vec![
            spec(Some(1000), Some(2), None),
            spec(Some(2000), Some(4), Some(500)),
        ];
        let total = sum_resource_usage(specs.iter());
        assert_eq!(total.memory_mb, Some(3000));
        assert_eq!(total.cpu_cores, Some(6));
        assert_eq!(total.gpu_vram_mb, Some(500));
    }

    #[test]
    fn sum_resource_usage_treats_missing_fields_as_zero() {
        let specs = vec![spec(Some(1000), None, None), spec(None, None, None)];
        let total = sum_resource_usage(specs.iter());
        assert_eq!(total.memory_mb, Some(1000));
        assert_eq!(total.cpu_cores, Some(0));
        assert_eq!(total.gpu_vram_mb, Some(0));
    }

    #[test]
    fn sum_resource_usage_empty_iterator_yields_zero() {
        let specs: Vec<ResourceSpec> = vec![];
        let total = sum_resource_usage(specs.iter());
        assert_eq!(total, ResourceSpec::default());
    }
}
