// app/steps/solutionSteps.ts
//
// Step wrapper around solution (procedural) memory formation so "use workflow"
// bodies — which can't touch the store/embeddings directly — can record HOW a
// task was solved. Best-effort: never throws back into the workflow.

import { recordSolution, type SolutionMeta } from "@/app/lib/solutionMemory";

export async function recordSolutionStep(args: {
  tenantId: string;
  meta: SolutionMeta;
}): Promise<void> {
  "use step";
  try {
    await recordSolution({ tenantId: args.tenantId, meta: args.meta });
  } catch {
    // formation must never break the workflow it hangs off of
  }
}
