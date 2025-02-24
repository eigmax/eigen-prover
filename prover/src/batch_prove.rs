use crate::traits::StageProver;
use crate::BatchContext;
use anyhow::Result;
use dsl_compile::circom_compiler;
use starky::prove::stark_prove;
use starky::{compressor12_exec::exec, compressor12_setup::setup};

#[derive(Default)]
pub struct BatchProver {}

impl BatchProver {
    pub fn new() -> Self {
        BatchProver {}
    }
}

impl StageProver for BatchProver {
    /// Generate stark proof and generate its verifier circuit in circom
    fn batch_prove(&self, ctx: &BatchContext) -> Result<()> {
        log::info!("start batch prove");
        // 1. stark prove: generate `.circom` file.
        let sp = &ctx.batch_stark;
        let c12_circom = &ctx.c12_circom;
        let c12_stark = &ctx.c12_stark;
        let r1_circom = &ctx.recursive1_circom; // output
        let r1_stark = &ctx.recursive1_stark; // output
        log::info!("batch_context: {:?}", ctx);
        stark_prove(
            &ctx.batch_struct,
            &sp.piljson,
            false,
            false,
            &sp.const_file,
            &sp.commit_file,
            &c12_circom.circom_file,
            &c12_stark.zkin,
            "", // prover address
        )?;

        // 2. Compile circom circuit to r1cs, and generate witness
        circom_compiler(
            c12_circom.circom_file.clone(),
            "goldilocks".to_string(), // prime
            "full".to_string(),       // full_simplification
            c12_circom.link_directories.clone(),
            c12_circom.output.clone(),
            false, // no_simplification
            false, // reduced_simplification
        )
        .unwrap();
        log::info!("end batch prove");

        log::info!("start c12 prove: {:?}", c12_stark);
        log::info!("1. compress setup");
        setup(
            &c12_stark.r1cs_file,
            &c12_stark.pil_file,
            &c12_stark.const_file,
            &c12_stark.exec_file,
            0,
        )?;

        let wasm_file = format!(
            "{}/{}.c12_js/{}.c12.wasm",
            c12_circom.output, ctx.task_name, ctx.task_name
        );
        log::info!("2. compress exec: {wasm_file}");
        exec(
            &c12_stark.zkin,
            &wasm_file,
            &c12_stark.pil_file,
            &c12_stark.exec_file,
            &c12_stark.commit_file,
        )?;

        // 3. stark prove
        stark_prove(
            &ctx.c12_struct,
            &c12_stark.piljson,
            true,
            true,
            &c12_stark.const_file,
            &c12_stark.commit_file,
            &r1_circom.circom_file,
            &r1_stark.zkin,
            "",
        )?;
        log::info!("end c12 prove");
        Ok(())
    }
}
