import { createSolarLabHelpers } from "./solarlab_code_execution_helpers.js";
import { createAndroidResponsesItemAdapter } from "./android_responses_items.js";

// A thin example of the preferred code-execution operating model:
// 1. reuse the last known visual context when possible
// 2. try a scripted Solar Lab journey first
// 3. escalate to one bounded freeform step only when needed

export function createSolarLabReviewSession(options = {}) {
  const {
    createSolarLabHelpersImpl = createSolarLabHelpers,
    createResponsesItemAdapterImpl = createAndroidResponsesItemAdapter,
    responsesItems = null,
    ...solarlabOptions
  } = options;
  const solarlab = createSolarLabHelpersImpl(solarlabOptions);
  const responsesItemAdapter =
    responsesItems === false
      ? null
      : createResponsesItemAdapterImpl(responsesItems ?? {});

  function buildScriptedContext(startingContext, scripted) {
    const finalArtifact = scripted.artifacts?.[scripted.artifacts.length - 1] ?? null;
    return {
      ...startingContext,
      screenshotPath: finalArtifact?.screenshot ?? startingContext.screenshotPath,
      uiDumpPath: finalArtifact?.ui_dump ?? startingContext.uiDumpPath,
      lastScenarioResult: scripted,
      scenarioArtifacts: scripted.artifacts ?? [],
      bundleDir: scripted.bundle_dir ?? null,
      manifestPath: scripted.manifest_path ?? null,
    };
  }

  async function buildResponseItems({
    startingContext,
    scripted,
    freeform,
    reviewLabel,
  }) {
    if (!responsesItemAdapter) {
      return null;
    }

    return {
      startingContext: await responsesItemAdapter.visualContextToInputItems(
        startingContext,
        { caption: `${reviewLabel} starting context` },
      ),
      scripted: await responsesItemAdapter.scenarioResultToInputItems(scripted, {
        caption: `${reviewLabel} scripted result`,
      }),
      freeformAfter: freeform?.after
        ? await responsesItemAdapter.visualContextToInputItems(freeform.after, {
            caption: `${reviewLabel} freeform result`,
          })
        : null,
    };
  }

  return {
    ...solarlab,

    async visualContextToInputItems(context, options = {}) {
      if (!responsesItemAdapter) {
        throw new Error("Responses item adapter was not configured for this review session");
      }
      return responsesItemAdapter.visualContextToInputItems(context, options);
    },

    async scenarioResultToInputItems(result, options = {}) {
      if (!responsesItemAdapter) {
        throw new Error("Responses item adapter was not configured for this review session");
      }
      return responsesItemAdapter.scenarioResultToInputItems(result, options);
    },

    async reviewFocusEarth({
      packageName = "com.sednalabs.solarlab",
      activity,
      freeformAction,
      includeResponseItems = false,
    } = {}) {
      const startingContext = await solarlab.ensureVisualContext("solarlab-review-start");
      const scripted = await solarlab.focusEarth({ packageName, activity });
      const scriptedContext = buildScriptedContext(startingContext, scripted);
      const freeform = freeformAction
        ? await solarlab.runExplorationStep(
            "solarlab-focus-earth-freeform",
            async (runtime) => freeformAction(runtime),
            { captureBefore: true, captureAfter: true },
          )
        : null;
      const responseItems = includeResponseItems
        ? await buildResponseItems({
            startingContext,
            scripted,
            freeform,
            reviewLabel: "Focus Earth review",
          })
        : null;

      return {
        strategy: "scenario-first",
        startingContext,
        scripted,
        scriptedContext,
        freeform,
        responseItems,
      };
    },

    async reviewImmersiveRoundtrip({
      packageName = "com.sednalabs.solarlab",
      activity,
      freeformAction,
      includeResponseItems = false,
    } = {}) {
      const startingContext = await solarlab.ensureVisualContext(
        "solarlab-immersive-review-start",
      );
      const scripted = await solarlab.immersiveRoundtrip({ packageName, activity });
      const scriptedContext = buildScriptedContext(startingContext, scripted);
      const freeform = freeformAction
        ? await solarlab.runExplorationStep(
            "solarlab-immersive-freeform",
            async (runtime) => freeformAction(runtime),
            { captureBefore: true, captureAfter: true },
          )
        : null;
      const responseItems = includeResponseItems
        ? await buildResponseItems({
            startingContext,
            scripted,
            freeform,
            reviewLabel: "Immersive review",
          })
        : null;

      return {
        strategy: "scenario-first",
        startingContext,
        scripted,
        scriptedContext,
        freeform,
        responseItems,
      };
    },
  };
}
