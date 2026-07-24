import { setTimeout as sleep } from "node:timers/promises";

function clamp(value, min, max) {
  return Math.min(Math.max(value, min), max);
}

function roundPoint(value) {
  return Math.round(value);
}

function normalizeRegion(region, deviceWidth, deviceHeight) {
  const left = clamp(region.left ?? 0, 0, deviceWidth);
  const top = clamp(region.top ?? 0, 0, deviceHeight);
  const width = clamp(region.width ?? deviceWidth, 1, deviceWidth - left);
  const height = clamp(region.height ?? deviceHeight, 1, deviceHeight - top);
  return { left, top, width, height };
}

function defaultView(deviceWidth, deviceHeight, frameWidth, frameHeight) {
  return {
    region: { left: 0, top: 0, width: deviceWidth, height: deviceHeight },
    frame: { width: frameWidth, height: frameHeight },
  };
}

export function createAndroidComputerBridge({
  runtime,
  deviceWidth,
  deviceHeight,
  frameWidth = deviceWidth,
  frameHeight = deviceHeight,
  cropScreenshot = null,
} = {}) {
  if (!runtime) {
    throw new Error("createAndroidComputerBridge requires a runtime");
  }
  if (!Number.isFinite(deviceWidth) || deviceWidth <= 0) {
    throw new Error("deviceWidth must be a positive number");
  }
  if (!Number.isFinite(deviceHeight) || deviceHeight <= 0) {
    throw new Error("deviceHeight must be a positive number");
  }
  if (!Number.isFinite(frameWidth) || frameWidth <= 0) {
    throw new Error("frameWidth must be a positive number");
  }
  if (!Number.isFinite(frameHeight) || frameHeight <= 0) {
    throw new Error("frameHeight must be a positive number");
  }

  const state = {
    device: { width: deviceWidth, height: deviceHeight },
    view: defaultView(deviceWidth, deviceHeight, frameWidth, frameHeight),
  };

  function getView() {
    return {
      device: { ...state.device },
      region: { ...state.view.region },
      frame: { ...state.view.frame },
      zoomed:
        state.view.region.left !== 0 ||
        state.view.region.top !== 0 ||
        state.view.region.width !== state.device.width ||
        state.view.region.height !== state.device.height,
    };
  }

  function remapPoint(x, y) {
    const {
      region: { left, top, width, height },
      frame,
    } = state.view;
    return {
      x: roundPoint(left + (x / frame.width) * width),
      y: roundPoint(top + (y / frame.height) * height),
    };
  }

  function swipePointsForScroll(action) {
    const anchor = remapPoint(action.x ?? state.view.frame.width / 2, action.y ?? state.view.frame.height / 2);
    const travel = clamp(
      Math.max(Math.abs(action.scroll_y ?? 0), Math.abs(action.scroll_x ?? 0), 120),
      120,
      Math.round(Math.min(state.view.region.width, state.view.region.height) * 0.7),
    );
    if (Math.abs(action.scroll_y ?? 0) >= Math.abs(action.scroll_x ?? 0)) {
      const direction = (action.scroll_y ?? 0) >= 0 ? -1 : 1;
      return {
        x1: anchor.x,
        y1: anchor.y,
        x2: anchor.x,
        y2: clamp(anchor.y + direction * travel, 0, state.device.height),
      };
    }
    const direction = (action.scroll_x ?? 0) >= 0 ? -1 : 1;
    return {
      x1: anchor.x,
      y1: anchor.y,
      x2: clamp(anchor.x + direction * travel, 0, state.device.width),
      y2: anchor.y,
    };
  }

  async function executeAction(action) {
    switch (action.type) {
      case "click": {
        const point = remapPoint(action.x, action.y);
        const result = await runtime.tap(point.x, point.y, action.tapOptions ?? {});
        return { type: action.type, devicePoint: point, result };
      }
      case "double_click": {
        const point = remapPoint(action.x, action.y);
        const result = typeof runtime.doubleTap === "function"
          ? await runtime.doubleTap(point.x, point.y, action.tapOptions ?? {})
          : {
              first: await runtime.tap(point.x, point.y, action.tapOptions ?? {}),
              second: await runtime.tap(point.x, point.y, action.tapOptions ?? {}),
              fallback: "runtime.doubleTap unavailable; dispatched as two tap actions",
            };
        return { type: action.type, devicePoint: point, result };
      }
      case "long_press": {
        const point = remapPoint(action.x, action.y);
        if (typeof runtime.longPress !== "function") {
          throw new Error("runtime.longPress is unavailable");
        }
        const result = await runtime.longPress(point.x, point.y, {
          durationMs: action.duration_ms ?? 500,
          waitForSelector: action.wait_for_selector ?? null,
          timeoutSecs: action.timeout_secs ?? 5,
        });
        return { type: action.type, devicePoint: point, result };
      }
      case "drag": {
        const start = remapPoint(action.x1, action.y1);
        const end = remapPoint(action.x2, action.y2);
        const result = await runtime.swipe(
          start.x,
          start.y,
          end.x,
          end.y,
          {
            durationMs: action.duration_ms,
            waitForSelector: action.wait_for_selector ?? null,
            expectScrollChange: action.expect_scroll_change ?? false,
            timeoutSecs: action.timeout_secs ?? 5,
          },
        );
        return { type: action.type, start, end, result };
      }
      case "scroll": {
        const swipe = swipePointsForScroll(action);
        const result = await runtime.swipe(
          swipe.x1,
          swipe.y1,
          swipe.x2,
          swipe.y2,
          {
            expectScrollChange: true,
            timeoutSecs: action.timeout_secs ?? 5,
          },
        );
        return { type: action.type, swipe, result };
      }
      case "keypress": {
        const keys = Array.isArray(action.keys) ? action.keys : [];
        const results = [];
        if (keys.length > 1 && typeof runtime.keyCombination === "function") {
          const keycodes = keys.map((key) => action.keyMap?.[key] ?? key);
          results.push(await runtime.keyCombination(keycodes, action.keyOptions ?? {}));
          return { type: action.type, keys, result: results };
        }
        for (const key of keys) {
          if (key.length === 1 && /[ -~]/.test(key)) {
            results.push(await runtime.typeText(key, action.typeOptions ?? {}));
          } else {
            results.push(
              await runtime.keyevent(action.keyMap?.[key] ?? `KEYCODE_${key.toUpperCase()}`, action.keyOptions ?? {}),
            );
          }
        }
        return { type: action.type, keys, result: results };
      }
      case "type": {
        const result = await runtime.typeText(action.text, action.typeOptions ?? {});
        return { type: action.type, text: action.text, result };
      }
      case "wait": {
        await sleep(action.ms ?? 2000);
        return { type: action.type, waitedMs: action.ms ?? 2000 };
      }
      case "zoom": {
        const region = normalizeRegion(action.region ?? {}, state.device.width, state.device.height);
        state.view = {
          region,
          frame: {
            width: action.frame?.width ?? state.view.frame.width,
            height: action.frame?.height ?? state.view.frame.height,
          },
        };
        return { type: action.type, view: getView() };
      }
      case "reset_zoom": {
        state.view = defaultView(
          state.device.width,
          state.device.height,
          state.view.frame.width,
          state.view.frame.height,
        );
        return { type: action.type, view: getView() };
      }
      default:
        throw new Error(`unsupported computer action: ${action.type}`);
    }
  }

  return {
    getView,

    setZoomRegion(region, frame = {}) {
      state.view = {
        region: normalizeRegion(region, state.device.width, state.device.height),
        frame: {
          width: frame.width ?? state.view.frame.width,
          height: frame.height ?? state.view.frame.height,
        },
      };
      return getView();
    },

    resetZoom() {
      state.view = defaultView(
        state.device.width,
        state.device.height,
        state.view.frame.width,
        state.view.frame.height,
      );
      return getView();
    },

    remapPoint,

    async captureView(label = "android-computer-view", { includeUi = false, display = true } = {}) {
      const captured = await runtime.captureState(label, { includeUi, display: false });
      const baseScreenshotPath = captured.screenshotPath;
      const view = getView();
      const croppedScreenshotPath =
        typeof cropScreenshot === "function"
          ? await cropScreenshot(baseScreenshotPath, view)
          : baseScreenshotPath;
      if (display && croppedScreenshotPath) {
        await runtime.displayScreenshot(croppedScreenshotPath);
      }
      return {
        ...captured,
        screenshotPath: croppedScreenshotPath,
        baseScreenshotPath,
        view,
      };
    },

    async executeActionBatch(actions, { captureAfter = false, captureLabel = "android-computer-batch" } = {}) {
      const results = [];
      for (const action of actions) {
        results.push(await executeAction(action));
      }
      const after = captureAfter ? await this.captureView(captureLabel) : null;
      return {
        actionsExecuted: actions.length,
        results,
        after,
        view: getView(),
      };
    },
  };
}
