use std::{env, fs, process::ExitCode};

use qq_maid_core::runtime::tools::knowledge::eval::{
    parse_dataset, run_fts5_baseline, run_knowledge_v3,
};

fn main() -> ExitCode {
    let arguments = env::args().skip(1).collect::<Vec<_>>();
    let semantic = arguments.iter().any(|argument| argument == "--semantic");
    let embedding_cache_dir = arguments
        .iter()
        .find_map(|argument| argument.strip_prefix("--embedding-cache="))
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| "cache/knowledge-embedding".into());
    let dataset_path = arguments
        .iter()
        .find(|argument| !argument.starts_with("--"))
        .cloned()
        .unwrap_or_else(|| {
            "qq-maid-core/src/runtime/tools/knowledge/fixtures/knowledge_eval_v1.json".to_owned()
        });
    let result = fs::read_to_string(&dataset_path)
        .map_err(|error| format!("failed to read {dataset_path}: {error}"))
        .and_then(|json| parse_dataset(&json))
        .and_then(|dataset| {
            if semantic {
                run_knowledge_v3(&dataset, embedding_cache_dir)
            } else {
                run_fts5_baseline(&dataset)
            }
        })
        .and_then(|report| {
            let passes = report.passes_correctness_gate();
            serde_json::to_string_pretty(&report)
                .map(|json| (json, passes))
                .map_err(|error| format!("failed to serialize eval report: {error}"))
        });
    match result {
        Ok((report, passes)) => {
            println!("{report}");
            if passes {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Err(error) => {
            eprintln!("knowledge eval failed: {error}");
            ExitCode::FAILURE
        }
    }
}
