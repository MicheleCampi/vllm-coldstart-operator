# A fix for vllm-project/vllm#69464 (study, not a gate)

Issue #69464: since v0.20.0 the vLLM CPU backend reserves a CPU for the
scheduler process (#36487). On a machine with 2 physical cores and one worker,
decode therefore runs on a single OpenMP thread (+61 % steady-state p50 on a
Xeon 6975P runner).

`fix-main.patch` is the proposed change against vLLM `main` at `5cfdcabb2`:

- `vllm/utils/ompmultiprocessing.py`: the default scheduler reservation is
  dropped when it would leave a worker with fewer than 2 CPUs. The CPUs
  reserved for the KV transfer thread (P/D) stay reserved. An explicit
  `VLLM_CPU_NUM_OF_RESERVED_CPU` is honored as before. A warning names the
  variable.
- `docs/getting_started/installation/cpu.md`: one sentence on the rule.
- `tests/utils_/test_cpu_resource_utils.py`: 6 unit tests on fake x86 SMT-2
  topologies (2, 3, 4 and 6 cores; 1 and 2 workers; explicit env; KV transfer).

Files:

- `main/` holds the patched files.
- `main-base/` holds the same files unpatched, so the tests can be shown to fail before the fix.
- `v0.34.0/ompmultiprocessing.py` is the same change applied to the v0.34.0 file, for the end-to-end run on the official image.

Workflow: `.github/workflows/vllm-69464-fix.yml`.

vLLM code is Apache-2.0, copyright the vLLM contributors.
