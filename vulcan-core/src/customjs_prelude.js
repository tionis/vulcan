// CustomJS compatibility (docs/specs/live-documents.md, 9.42.2).
//
// Classes load on first use of `customJS`, `cJS()`, or
// `forceLoadCustomJS()`, so sessions that never touch them pay nothing.
// Each file is one class expression, evaluated as `(` + source + `)` like the
// CustomJS plugin does; its instance is stored under the class name.
(() => {
  let loaded = null;
  const state = {};

  const load = () => {
    if (loaded !== null) {
      return loaded;
    }
    const sources = JSON.parse(__vulcan_customjs_sources());
    const customJS = { state };
    state._ready = false;
    // Set before evaluating, so a constructor that reads `customJS` sees the
    // classes loaded so far instead of recursing.
    loaded = customJS;
    for (const skipped of sources.skipped || []) {
      console.error(`CustomJS couldn't import ${skipped.path}: ${skipped.reason}`);
    }
    for (const file of sources.files) {
      try {
        const definition = (0, eval)(`(${file.source}\n)`);
        const instance = new definition();
        customJS[instance.constructor.name] = instance;
        customJS[`create${definition.name}Instance`] = () => new definition();
      } catch (error) {
        console.error(`CustomJS couldn't import ${file.path}: ${error}`);
      }
    }
    state._ready = true;
    return customJS;
  };

  Object.defineProperty(globalThis, "customJS", {
    configurable: true,
    enumerable: false,
    get: load,
    set(value) {
      loaded = value;
    },
  });

  globalThis.cJS = async (selection) => {
    const customJS = load();
    if (typeof selection === "string") {
      return customJS[selection];
    }
    if (typeof selection === "function") {
      await selection(customJS);
    }
    return customJS;
  };

  globalThis.forceLoadCustomJS = async () => {
    loaded = null;
    load();
  };

  // Vulcan-only: the CustomJS method behind a DQL function, for the DQL bridge.
  globalThis.__vulcanCustomJsCall = (target, args) => {
    const dot = target.indexOf(".");
    const className = target.slice(0, dot);
    const methodName = target.slice(dot + 1);
    const instance = load()[className];
    if (instance === undefined) {
      throw new Error(`CustomJS class \`${className}\` is not loaded`);
    }
    if (typeof instance[methodName] !== "function") {
      throw new Error(`CustomJS class \`${className}\` has no method \`${methodName}\``);
    }
    const result = instance[methodName](...args);
    if (result !== null && typeof result === "object" && typeof result.then === "function") {
      throw new Error(`DQL function \`${target}\` must return a value, not a Promise`);
    }
    return result;
  };
})();
