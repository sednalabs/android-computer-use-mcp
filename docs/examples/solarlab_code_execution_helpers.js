import { createAndroidEmulatorRuntime } from "./android_emulator_runtime_client.js";

// Thin Solar Lab overlay for the generic persistent Android runtime.

export function createSolarLabHelpers(options = {}) {
  const runtime = createAndroidEmulatorRuntime(options);

  return {
    ...runtime,

    async screenshot(label = "solarlab-shot") {
      return runtime.captureScreenshot(label);
    },

    async dumpUi(label = "solarlab-ui") {
      return runtime.dumpUiHierarchy(label);
    },

    async focusEarth({ packageName = "com.sednalabs.solarlab", activity } = {}) {
      return runtime.runSolarLabScenario("stage_first_focus_earth", {
        package_name: packageName,
        activity,
      });
    },

    async immersiveRoundtrip({
      packageName = "com.sednalabs.solarlab",
      activity,
    } = {}) {
      return runtime.runSolarLabScenario("stage_first_immersive_roundtrip", {
        package_name: packageName,
        activity,
      });
    },
  };
}
