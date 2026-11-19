// Bounded geometry-only summaries. Never retain device names, window titles,
// display unique IDs or unrelated settings from the raw Android dumps.
const boundedLines = (output) => String(output).slice(0, 1024 * 1024).split(/\r?\n/).slice(0, 8192)
  .filter((line) => line.length <= 2048);
const number = "-?\\d{1,10}(?:\\.\\d{1,9})?";
const scalar = (line, name) => {
  const match = line.match(new RegExp(`\\b${name}\\s*[:=]\\s*(${number})(?=[,\\s}\\]]|$)`));
  return match ? Number(match[1]) : undefined;
};
const frame = (line, name) => {
  const match = line.match(new RegExp(`${name}=(?:\\[|Rect\\()(-?\\d{1,10}),\\s*(-?\\d{1,10})(?:,\\s*|\\s+-\\s+)(-?\\d{1,10}),\\s*(-?\\d{1,10})(?:\\]|\\))`));
  return match ? match.slice(1).map(Number) : undefined;
};
function viewport(line) {
  if (!/\b(?:DisplayViewport\{|Viewport (?:INTERNAL|EXTERNAL|VIRTUAL):)/.test(line)) return undefined;
  const displayId = scalar(line, "displayId");
  if (displayId === undefined) return undefined;
  const deviceSize = line.match(/\bdeviceSize=\[(\d{1,10}),\s*(\d{1,10})\]/);
  const active = line.match(/\bisActive=(?:\[([01])\]|(true|false))/);
  const valid = line.match(/\bvalid=(true|false)/);
  return {
    display_id: displayId, orientation: scalar(line, "orientation"),
    logical_frame: frame(line, "logicalFrame"), physical_frame: frame(line, "physicalFrame"),
    device_size: deviceSize ? deviceSize.slice(1).map(Number)
      : [scalar(line, "deviceWidth"), scalar(line, "deviceHeight")].every((value) => value !== undefined)
        ? [scalar(line, "deviceWidth"), scalar(line, "deviceHeight")] : undefined,
    active: active ? active[1] === "1" || active[2] === "true" : undefined,
    valid: valid ? valid[1] === "true" : undefined,
  };
}
const viewports = (lines) => {
  const unique = new Map();
  for (const line of lines) {
    for (const part of line.split(/(?=DisplayViewport\{)/)) {
      const item = viewport(part);
      if (item) unique.set(JSON.stringify(item), item);
      if (unique.size >= 16) break;
    }
    if (unique.size >= 16) break;
  }
  return [...unique.values()];
};
export function summarizeWmSize(output) {
  return Object.fromEntries(boundedLines(output).flatMap((line) => {
    const match = line.trim().match(/^(Physical|Override) size: (\d{1,6})x(\d{1,6})$/);
    return match ? [[match[1].toLowerCase(), [Number(match[2]), Number(match[3])]]] : [];
  }));
}
export function summarizeInputGeometry(output) {
  const lines = boundedLines(output);
  const mappers = [];
  let mapper;
  let mapperIndent;
  let deviceId;
  let section;
  let sectionIndent;
  for (const line of lines) {
    const trimmed = line.trim();
    if (!trimmed) continue;
    const indent = line.length - line.trimStart().length;
    const device = trimmed.match(/^Device (\d{1,6}):/);
    if (device) { deviceId = Number(device[1]); mapper = undefined; }
    const header = trimmed.match(/^Touch Input Mapper(?: \(mode - (DIRECT|DISABLED|UNSCALED|NAVIGATION|POINTER)\))?:$/);
    if (header) {
      mapper = { device_id: deviceId, mode: header[1], raw_axes: {}, affine: {} };
      mapperIndent = indent;
      section = undefined;
      if (mappers.length < 32) mappers.push(mapper);
      else mapper = undefined;
      continue;
    }
    if (!mapper) continue;
    if (indent <= mapperIndent) { mapper = undefined; continue; }
    if (section && indent <= sectionIndent) section = undefined;
    if (/^(Raw Touch Axes|Affine Transformation):$/.test(trimmed)) {
      section = trimmed.slice(0, -1); sectionIndent = indent; continue;
    }
    const currentViewport = viewport(trimmed);
    if (currentViewport) mapper.viewport = currentViewport;
    const orientation = trimmed.match(/^Orientation:\s*(.*)$/);
    if (orientation) {
      // Android 15 emits enum enumerators (Rotation0), not the ROTATION_0 alias.
      // Preserve a short explicit token; absence or another shape remains unknown.
      mapper.orientation_present = true;
      if (/^[A-Za-z0-9_]{1,32}$/.test(orientation[1])) mapper.Orientation = orientation[1];
    }
    const parameter = trimmed.match(/^(OrientationAware|GestureMode|DeviceType): (true|false|MULTI_TOUCH|SINGLE_TOUCH|TOUCH_SCREEN|TOUCH_PAD|POINTER)$/);
    if (parameter) mapper[parameter[1]] = parameter[2];
    if (section === "Raw Touch Axes") {
      const axis = trimmed.match(/^([XY]): /);
      if (axis) {
        mapper.raw_axes[axis[1]] = Object.fromEntries(["min", "max", "flat", "fuzz", "resolution"]
          .map((name) => [name, scalar(trimmed, name)]).filter(([, value]) => value !== undefined));
      }
    }
    if (section === "Affine Transformation") {
      const coefficient = trimmed.match(new RegExp(`^([XY] (?:scale|ymix|xmix|offset)): (${number})$`));
      if (coefficient) mapper.affine[coefficient[1]] = Number(coefficient[2]);
    }
  }
  return {
    viewports: viewports(lines),
    touch_mappers: mappers.sort((left, right) => Number(right.viewport?.display_id === 0 && right.mode === "DIRECT")
      - Number(left.viewport?.display_id === 0 && left.mode === "DIRECT")).slice(0, 8),
  };
}
export function summarizeDisplayGeometry(output) {
  const lines = boundedLines(output);
  const logicalDisplays = [];
  for (const line of lines) {
    const info = line.trim().match(/^(mBaseDisplayInfo|mOverrideDisplayInfo|mDisplayInfo)=DisplayInfo\{/);
    if (!info || logicalDisplays.length >= 16) continue;
    const real = line.match(/\breal (\d{1,6}) x (\d{1,6})/);
    const app = line.match(/\bapp (\d{1,6}) x (\d{1,6})/);
    const rotation = line.match(/\brotation ([0-3])(?:,|\s)/);
    const displayId = line.match(/\bdisplayId (\d{1,6})(?:,|\s)/);
    logicalDisplays.push({
      source: info[1], display_id: displayId ? Number(displayId[1]) : undefined,
      real_size: real ? real.slice(1).map(Number) : undefined,
      app_size: app ? app.slice(1).map(Number) : undefined,
      rotation: rotation ? Number(rotation[1]) : undefined,
    });
  }
  return { viewports: viewports(lines), logical_displays: logicalDisplays };
}
