//! Terraform CLI output compression.
//!
//! Filters verbose plan diffs, apply logs, and state lists.
//! Specialized handlers for: plan, apply, destroy, state, output, init, fmt, validate.

use crate::tracking;
use anyhow::{Context, Result};
use lazy_static::lazy_static;
use regex::Regex;
use std::process::Command;

lazy_static! {
    static ref TF_RESOURCE_WILL: Regex =
        Regex::new(r"# (\S+) will be (created|updated in-place|destroyed|replaced)").unwrap();
    static ref TF_PLAN_SUMMARY: Regex = Regex::new(r"^Plan: \d+ to add").unwrap();
    static ref TF_APPLY_SUMMARY: Regex = Regex::new(r"^Apply complete!").unwrap();
    static ref TF_DESTROY_SUMMARY: Regex = Regex::new(r"^Destroy complete!").unwrap();
    static ref TF_OUTPUTS_SECTION: Regex = Regex::new(r"^Outputs:").unwrap();
    static ref TF_OUTPUT_VALUE: Regex = Regex::new(r"^\S+ = ").unwrap();
    static ref TF_STATE_RESOURCE: Regex = Regex::new(r"^(\S+)$").unwrap();
    static ref TF_MODULE_PREFIX: Regex = Regex::new(r"^module\.([^.]+)").unwrap();
}

/// Run a Terraform command with token-optimized output.
pub fn run(subcommand: &str, args: &[String], verbose: u8) -> Result<()> {
    match subcommand {
        "plan" => run_plan(args, verbose),
        "apply" => run_apply(args, verbose),
        "destroy" => run_destroy(args, verbose),
        "state" => run_state(args, verbose),
        "output" | "init" | "fmt" | "validate" => run_passthrough(subcommand, args, verbose),
        _ => run_passthrough(subcommand, args, verbose),
    }
}

fn run_terraform(
    subcommand: &str,
    args: &[String],
    verbose: u8,
) -> Result<(String, String, std::process::ExitStatus)> {
    let mut cmd = Command::new("terraform");
    cmd.arg(subcommand);
    for arg in args {
        cmd.arg(arg);
    }

    if verbose > 0 {
        eprintln!("Running: terraform {} {}", subcommand, args.join(" "));
    }

    let output = cmd
        .output()
        .context(format!("Failed to run terraform {}", subcommand))?;
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    if !output.status.success() {
        eprintln!("{}", stderr.trim());
    }

    Ok((stdout, stderr, output.status))
}

fn run_plan(args: &[String], verbose: u8) -> Result<()> {
    let timer = tracking::TimedExecution::start();
    let (raw, stderr, status) = run_terraform("plan", args, verbose)?;

    if !status.success() {
        timer.track("terraform plan", "rtk terraform plan", &stderr, &stderr);
        eprintln!("{}", stderr.trim());
        std::process::exit(status.code().unwrap_or(1));
    }

    let filtered = match filter_plan(&raw) {
        Some(f) => {
            println!("{}", f);
            f
        }
        None => {
            print!("{}", raw);
            raw.clone()
        }
    };

    timer.track("terraform plan", "rtk terraform plan", &raw, &filtered);
    Ok(())
}

fn run_apply(args: &[String], verbose: u8) -> Result<()> {
    let timer = tracking::TimedExecution::start();
    let (raw, stderr, status) = run_terraform("apply", args, verbose)?;

    if !status.success() {
        timer.track("terraform apply", "rtk terraform apply", &stderr, &stderr);
        eprintln!("{}", stderr.trim());
        std::process::exit(status.code().unwrap_or(1));
    }

    let filtered = match filter_apply(&raw) {
        Some(f) => {
            println!("{}", f);
            f
        }
        None => {
            print!("{}", raw);
            raw.clone()
        }
    };

    timer.track("terraform apply", "rtk terraform apply", &raw, &filtered);
    Ok(())
}

fn run_destroy(args: &[String], verbose: u8) -> Result<()> {
    let timer = tracking::TimedExecution::start();
    let (raw, stderr, status) = run_terraform("destroy", args, verbose)?;

    if !status.success() {
        timer.track(
            "terraform destroy",
            "rtk terraform destroy",
            &stderr,
            &stderr,
        );
        eprintln!("{}", stderr.trim());
        std::process::exit(status.code().unwrap_or(1));
    }

    let filtered = match filter_destroy(&raw) {
        Some(f) => {
            println!("{}", f);
            f
        }
        None => {
            print!("{}", raw);
            raw.clone()
        }
    };

    timer.track(
        "terraform destroy",
        "rtk terraform destroy",
        &raw,
        &filtered,
    );
    Ok(())
}

fn run_state(args: &[String], verbose: u8) -> Result<()> {
    let timer = tracking::TimedExecution::start();

    let sub = args.first().map(|s| s.as_str()).unwrap_or("");
    let rest = if args.is_empty() { &[] } else { &args[1..] };

    let full_sub = if args.is_empty() {
        "state".to_string()
    } else {
        format!("state {}", args.join(" "))
    };

    let (raw, stderr, status) = run_terraform("state", args, verbose)?;

    if !status.success() {
        timer.track(
            &format!("terraform {}", full_sub),
            &format!("rtk terraform {}", full_sub),
            &stderr,
            &stderr,
        );
        eprintln!("{}", stderr.trim());
        std::process::exit(status.code().unwrap_or(1));
    }

    // Only filter `state list`; other state subcommands pass through
    let filtered = if sub == "list" {
        match filter_state_list(&raw) {
            Some(f) => {
                println!("{}", f);
                f
            }
            None => {
                print!("{}", raw);
                raw.clone()
            }
        }
    } else {
        let _ = rest; // suppress unused warning
        print!("{}", raw);
        raw.clone()
    };

    timer.track(
        &format!("terraform {}", full_sub),
        &format!("rtk terraform {}", full_sub),
        &raw,
        &filtered,
    );
    Ok(())
}

fn run_passthrough(subcommand: &str, args: &[String], verbose: u8) -> Result<()> {
    let timer = tracking::TimedExecution::start();

    let mut cmd = Command::new("terraform");
    cmd.arg(subcommand);
    for arg in args {
        cmd.arg(arg);
    }

    if verbose > 0 {
        eprintln!("Running: terraform {} {}", subcommand, args.join(" "));
    }

    let output = cmd
        .output()
        .context(format!("Failed to run terraform {}", subcommand))?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let full_cmd = format!("terraform {} {}", subcommand, args.join(" "));

    print!("{}", stdout);
    if !output.status.success() {
        eprintln!("{}", stderr.trim());
    }

    timer.track_passthrough(&full_cmd, &format!("rtk {}", full_cmd));

    if !output.status.success() {
        std::process::exit(output.status.code().unwrap_or(1));
    }

    Ok(())
}

// --- Filter functions ---

/// Filter `terraform plan` output: group resources by module, show action symbols.
fn filter_plan(output: &str) -> Option<String> {
    // Collect resource changes
    let mut root_resources: Vec<(String, String)> = Vec::new(); // (action, address)
    let mut module_resources: std::collections::BTreeMap<String, Vec<(String, String)>> =
        std::collections::BTreeMap::new();
    let mut summary_line = String::new();

    for line in output.lines() {
        if let Some(caps) = TF_RESOURCE_WILL.captures(line) {
            let address = caps[1].to_string();
            let action_str = &caps[2];
            let symbol = match action_str {
                "created" => "[+]",
                "updated in-place" => "[~]",
                "destroyed" => "[-]",
                "replaced" => "[+/-]",
                _ => "[?]",
            };

            // Group by module
            if let Some(mod_caps) = TF_MODULE_PREFIX.captures(&address) {
                let module_name = format!("module.{}", &mod_caps[1]);
                module_resources
                    .entry(module_name)
                    .or_default()
                    .push((symbol.to_string(), address));
            } else {
                root_resources.push((symbol.to_string(), address));
            }
        } else if TF_PLAN_SUMMARY.is_match(line) {
            summary_line = line.trim().to_string();
        }
    }

    // Nothing found → return None so caller prints raw
    if root_resources.is_empty() && module_resources.is_empty() && summary_line.is_empty() {
        return None;
    }

    let mut result = Vec::new();

    if !root_resources.is_empty() {
        result.push("root:".to_string());
        for (symbol, address) in &root_resources {
            result.push(format!("  {} {}", symbol, address));
        }
    }

    for (module, resources) in &module_resources {
        result.push(format!("{}:", module));
        for (symbol, address) in resources {
            result.push(format!("  {} {}", symbol, address));
        }
    }

    if !summary_line.is_empty() {
        result.push(summary_line);
    }

    Some(result.join("\n"))
}

/// Filter `terraform apply` output: show summary and outputs.
fn filter_apply(output: &str) -> Option<String> {
    let mut result: Vec<String> = Vec::new();
    let mut in_outputs = false;
    let mut saw_output_value = false;

    for line in output.lines() {
        if TF_APPLY_SUMMARY.is_match(line) {
            result.push(line.trim().to_string());
        } else if TF_OUTPUTS_SECTION.is_match(line.trim()) {
            in_outputs = true;
            saw_output_value = false;
            result.push(line.trim().to_string());
        } else if in_outputs {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                // Terraform emits a blank line between "Outputs:" and values;
                // only stop collecting once we've already seen at least one value.
                if saw_output_value {
                    in_outputs = false;
                }
            } else if TF_OUTPUT_VALUE.is_match(trimmed) {
                saw_output_value = true;
                result.push(trimmed.to_string());
            }
        }
    }

    if result.is_empty() {
        None
    } else {
        Some(result.join("\n"))
    }
}

/// Filter `terraform destroy` output: show destroyed count.
fn filter_destroy(output: &str) -> Option<String> {
    let mut result: Vec<String> = Vec::new();

    for line in output.lines() {
        if TF_DESTROY_SUMMARY.is_match(line) || TF_PLAN_SUMMARY.is_match(line) {
            result.push(line.trim().to_string());
        }
    }

    if result.is_empty() {
        None
    } else {
        Some(result.join("\n"))
    }
}

/// Filter `terraform state list` output: group by module, show type summary only.
///
/// Resource names are elided; only type names are kept to achieve ≥60% savings.
fn filter_state_list(output: &str) -> Option<String> {
    let mut root_types: Vec<String> = Vec::new();
    let mut module_types: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();

    for line in output.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        // Extract the resource type (first two dot-separated segments after any module. prefix).
        // e.g. "module.ecs.aws_ecs_cluster.main" → type = "aws_ecs_cluster"
        //      "aws_vpc.main"                   → type = "aws_vpc"
        let type_name = if let Some(mod_caps) = TF_MODULE_PREFIX.captures(line) {
            let module_name = format!("module.{}", &mod_caps[1]);
            // strip "module.X." prefix to get "aws_type.name"
            let without_module = &line[module_name.len() + 1..]; // +1 for the dot
            let rtype = without_module.split('.').next().unwrap_or(without_module);
            module_types
                .entry(module_name)
                .or_default()
                .push(rtype.to_string());
            continue;
        } else {
            line.split('.').next().unwrap_or(line).to_string()
        };
        if !root_types.contains(&type_name) {
            root_types.push(type_name);
        }
    }

    if root_types.is_empty() && module_types.is_empty() {
        return None;
    }

    let mut result = Vec::new();

    if !root_types.is_empty() {
        // Deduplicate types
        root_types.dedup();
        result.push(format!("root: {}", root_types.join(", ")));
    }

    for (module, types) in &module_types {
        let mut deduped = types.clone();
        deduped.sort();
        deduped.dedup();
        result.push(format!("{}: {}", module, deduped.join(", ")));
    }

    Some(result.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn count_tokens(text: &str) -> usize {
        text.split_whitespace().count()
    }

    #[test]
    fn test_filter_plan_basic() {
        let input = r#"
Terraform will perform the following actions:

  # aws_vpc.main will be created
  + resource "aws_vpc" "main" {
      + cidr_block = "10.0.0.0/16"
    }

  # module.ecs.aws_ecs_cluster.main will be created
  + resource "aws_ecs_cluster" "main" {
      + name = "my-cluster"
    }

  # module.ecs.aws_ecs_task_definition.api will be updated in-place
  ~ resource "aws_ecs_task_definition" "api" {
      ~ cpu = "256" -> "512"
    }

  # module.rds.aws_rds_cluster.main will be created
  + resource "aws_rds_cluster" "main" {
      + cluster_identifier = "mydb"
    }

Plan: 3 to add, 1 to change, 0 to destroy.
"#;
        let result = filter_plan(input).unwrap();
        assert!(result.contains("root:"));
        assert!(result.contains("[+] aws_vpc.main"));
        assert!(result.contains("module.ecs:"));
        assert!(result.contains("[+] module.ecs.aws_ecs_cluster.main"));
        assert!(result.contains("[~] module.ecs.aws_ecs_task_definition.api"));
        assert!(result.contains("module.rds:"));
        assert!(result.contains("Plan: 3 to add, 1 to change, 0 to destroy."));
    }

    #[test]
    fn test_filter_plan_no_changes() {
        let input = "No changes. Your infrastructure matches the configuration.";
        let result = filter_plan(input);
        assert!(result.is_none());
    }

    #[test]
    fn test_filter_plan_destroy() {
        let input = r#"
  # aws_s3_bucket.old will be destroyed
  - resource "aws_s3_bucket" "old" {
      - bucket = "my-old-bucket"
    }

Plan: 0 to add, 0 to change, 1 to destroy.
"#;
        let result = filter_plan(input).unwrap();
        assert!(result.contains("[-] aws_s3_bucket.old"));
        assert!(result.contains("Plan:"));
    }

    #[test]
    fn test_filter_apply_with_outputs() {
        let input = r#"
aws_vpc.main: Creating...
aws_vpc.main: Creation complete after 2s [id=vpc-abc123]

Apply complete! Resources: 1 added, 0 changed, 0 destroyed.

Outputs:

vpc_id = "vpc-abc123"
region = "us-east-1"
"#;
        let result = filter_apply(input).unwrap();
        assert!(result.contains("Apply complete!"));
        assert!(result.contains("Outputs:"));
        assert!(result.contains("vpc_id = \"vpc-abc123\""));
        assert!(!result.contains("Creating..."));
        assert!(!result.contains("Creation complete"));
    }

    #[test]
    fn test_filter_apply_no_outputs() {
        let input = r#"
aws_vpc.main: Creating...
aws_vpc.main: Creation complete after 2s

Apply complete! Resources: 1 added, 0 changed, 0 destroyed.
"#;
        let result = filter_apply(input).unwrap();
        assert!(result.contains("Apply complete!"));
        assert!(!result.contains("Creating..."));
    }

    #[test]
    fn test_filter_apply_empty() {
        let result = filter_apply("no useful output here");
        assert!(result.is_none());
    }

    #[test]
    fn test_filter_destroy_basic() {
        let input = r#"
aws_s3_bucket.old: Destroying...
aws_s3_bucket.old: Destruction complete after 1s

Destroy complete! Resources: 1 destroyed.
"#;
        let result = filter_destroy(input).unwrap();
        assert!(result.contains("Destroy complete!"));
        assert!(!result.contains("Destroying..."));
    }

    #[test]
    fn test_filter_state_list_basic() {
        let input = r#"aws_vpc.main
aws_subnet.public
module.ecs.aws_ecs_cluster.main
module.ecs.aws_ecs_service.api
module.rds.aws_rds_cluster.main
"#;
        let result = filter_state_list(input).unwrap();
        assert!(result.contains("root:"));
        assert!(result.contains("aws_vpc"));
        assert!(result.contains("module.ecs:"));
        assert!(result.contains("aws_ecs_cluster"));
        assert!(result.contains("module.rds:"));
        assert!(result.contains("aws_rds_cluster"));
        // Must not contain the resource instance names (savings come from dropping ".main")
        assert!(!result.contains("aws_vpc.main"));
    }

    #[test]
    fn test_filter_state_list_deduplicates_types() {
        let input = "module.big.aws_resource_a.item1\nmodule.big.aws_resource_a.item2\nmodule.big.aws_resource_b.item3\n";
        let result = filter_state_list(input).unwrap();
        // Type "aws_resource_a" should appear only once
        let count = result.matches("aws_resource_a").count();
        assert_eq!(count, 1, "duplicate type should be deduplicated");
        assert!(result.contains("aws_resource_b"));
    }

    #[test]
    fn test_filter_state_list_empty() {
        let result = filter_state_list("");
        assert!(result.is_none());
    }

    #[test]
    fn test_plan_token_savings() {
        let input = include_str!("../tests/fixtures/terraform_plan_raw.txt");
        let result = filter_plan(input).unwrap();
        let savings = 100.0 - (count_tokens(&result) as f64 / count_tokens(input) as f64 * 100.0);
        assert!(
            savings >= 60.0,
            "Terraform plan filter: expected >=60% savings, got {:.1}%",
            savings
        );
    }

    #[test]
    fn test_state_list_token_savings() {
        let input = include_str!("../tests/fixtures/terraform_state_list_raw.txt");
        let result = filter_state_list(input).unwrap();
        let savings = 100.0 - (count_tokens(&result) as f64 / count_tokens(input) as f64 * 100.0);
        assert!(
            savings >= 60.0,
            "Terraform state list filter: expected >=60% savings, got {:.1}%",
            savings
        );
    }
}
