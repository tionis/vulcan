// Numerals calculator blocks for Vulcan, evaluated by the bundled mathjs.
//
// Ported from Numerals 1.10.2 (https://github.com/gtg922r/obsidian-numerals),
// MIT License, Copyright (c) 2022-2025 The Numerals contributors: the
// directive preprocessor, the line evaluator with @prev and @sum/@total, the
// frontmatter scope, cross-note references, and result formatting. QuickJS
// has no Intl, so the locale number formats are written out here; the
// "system" format is the comma-thousands, period-decimal one.
//
// Rust drives it through `globalThis.__numerals` with JSON strings:
// configure(settings), beginNote(metadata, references), block(source, style),
// inline(expression, mode, style, continuesSection), and pristine().
(function () {
  "use strict";
  const math = globalThis.math;
  // Vulcan reuses a loaded engine for later notes. Expressions can change
  // mathjs itself (`createUnit(...)`, `config(...)`, `import(...)`), which
  // must not leak into another note, so these record that they ran and
  // pristine() tells Rust to discard the engine.
  const original = { createUnit: math.createUnit, config: math.config, import: math.import };
  let changed = false;
  const guarded = {};
  for (const name of Object.keys(original)) {
    guarded[name] = function () {
      if (name !== "config" || arguments.length > 0) changed = true;
      return original[name].apply(math, arguments);
    };
  }
  original.import.call(math, guarded, { override: true });
  const MAX_FIXED_FORMAT_LEADING_DECIMAL_ZEROES = 5;
  const MIN_DECIMAL_PLACES = 0;
  const MAX_DECIMAL_PLACES = 20;

  const defaultCurrencyMap = [
    { symbol: "$", name: "dollar", currency: "USD" },
    { symbol: "€", name: "euro", currency: "EUR" },
    { symbol: "£", name: "pound", currency: "GBP" },
    { symbol: "¥", name: "yen", currency: "JPY" },
    { symbol: "₹", name: "rupee", currency: "INR" },
  ];
  // ISO 4217 minor units for codes that do not use two.
  const currencyFractionDigits = {
    BHD: 3, BIF: 0, CLP: 0, DJF: 0, GNF: 0, IQD: 3, ISK: 0, JOD: 3, JPY: 0,
    KMF: 0, KRW: 0, KWD: 3, LYD: 3, OMR: 3, PYG: 0, RWF: 0, TND: 3, UGX: 0,
    UYI: 0, VND: 0, VUV: 0, XAF: 0, XOF: 0, XPF: 0,
  };
  const formatAliases = {
    system: "system", fixed: "fixed", exponential: "exponential",
    scientific: "exponential", engineering: "engineering",
    "comma-period": "comma-period", "period-comma": "period-comma",
    "space-comma": "space-comma", indian: "indian",
  };
  const locales = {
    system: { group: ",", decimal: "." },
    "comma-period": { group: ",", decimal: "." },
    "period-comma": { group: ".", decimal: "," },
    "space-comma": { group: " ", decimal: "," },
    indian: { group: ",", decimal: ".", indian: true },
  };

  let settings = {
    number_format: "system",
    currency_precision: "currency-standard",
    currency_display: "code",
    currency_decimal_places: 2,
    force_all: false,
    hide_emitter_markup: true,
    hide_non_emitters: true,
    equation_separator: " = ",
    cross_note_references: true,
  };
  let preProcessors = [];
  let currencies = [];
  let createdUnits = new Set();
  let noteMetadata = {};
  let noteGlobals = new Map();
  let references = {};
  let prevInline;

  function configure(json) {
    settings = Object.assign(settings, JSON.parse(json));
    let currencyMap = defaultCurrencyMap.map((entry) => Object.assign({}, entry));
    for (const entry of currencyMap) {
      if (entry.symbol === "$" && settings.dollar_currency) entry.currency = settings.dollar_currency;
      if (entry.symbol === "¥" && settings.yen_currency) entry.currency = settings.yen_currency;
    }
    const custom = settings.custom_currency;
    if (custom && custom.symbol && custom.currency) {
      const replacement = { symbol: custom.symbol, name: custom.name || "custom", currency: custom.currency, custom: true };
      currencyMap = currencyMap.map((entry) => (entry.symbol === custom.symbol ? replacement : entry));
      if (!currencyMap.some((entry) => entry.symbol === custom.symbol)) currencyMap.push(replacement);
    }
    for (const entry of currencyMap) {
      if (entry.currency && !createdUnits.has(entry.currency)) {
        try {
          original.createUnit.call(math, entry.currency, { aliases: [entry.currency.toLowerCase(), entry.symbol] });
        } catch (_) {
          // The unit exists already, or the symbol is not a valid alias.
        }
        createdUnits.add(entry.currency);
      }
    }
    currencies = [];
    for (const entry of currencyMap) {
      let unit;
      try {
        unit = math.unit(entry.currency);
      } catch (_) {
        continue;
      }
      const digits = entry.custom
        ? settings.currency_decimal_places
        : (currencyFractionDigits[entry.currency] !== undefined ? currencyFractionDigits[entry.currency] : 2);
      currencies.push({ code: entry.currency, symbol: entry.symbol, name: entry.name, unit, fractionDigits: digits });
    }
    preProcessors = [{ regex: /,(\d{3})/g, replaceStr: "$1" }].concat(
      currencyMap.map((entry) => ({
        regex: RegExp("\\" + entry.symbol + "([\\d\\.]+)", "g"),
        replaceStr: "$1 " + entry.currency,
      })),
    );
  }

  function beginNote(metadataJson, referencesJson) {
    noteMetadata = JSON.parse(metadataJson) || {};
    references = JSON.parse(referencesJson) || {};
    noteGlobals = new Map();
    prevInline = undefined;
  }

  function replaceStrings(text, maps) {
    for (const processor of maps) text = text.replace(processor.regex, processor.replaceStr);
    return text;
  }

  // ---- format directives (@format, @decimalPlaces) ----

  function parseFormatDirectiveLine(source, lineIndex) {
    const match = source.match(/^[\t ]*@(format|decimalPlaces?)(?=$|[\t \r])([\s\S]*?)[\t ]*\r?$/i);
    if (!match) return { status: "not-directive" };
    const command = match[1].toLowerCase();
    const value = match[2].trim();
    const invalid = (kind, message) => ({ status: "invalid", error: { kind, lineIndex, source, value, message } });
    if (command === "format") {
      if (value.length === 0) return invalid("format", "Missing @format value.");
      const format = formatAliases[value.toLowerCase()];
      if (!format) return invalid("format", `Unknown @format value: ${value}.`);
      return { status: "valid", directive: { kind: "format", value: format } };
    }
    const range = `expected an integer from ${MIN_DECIMAL_PLACES} to ${MAX_DECIMAL_PLACES}.`;
    if (value.length === 0) return invalid("decimalPlaces", `Missing @decimalPlaces value; ${range}`);
    if (!/^\d+$/.test(value)) return invalid("decimalPlaces", `Invalid @decimalPlaces value: ${value}; ${range}`);
    const places = Number(value);
    if (places < MIN_DECIMAL_PLACES || places > MAX_DECIMAL_PLACES) {
      return invalid("decimalPlaces", `Invalid @decimalPlaces value: ${value}; ${range}`);
    }
    return { status: "valid", directive: { kind: "decimalPlaces", value: places } };
  }

  function collectFormatDirectives(source) {
    const directiveLineIndexes = [];
    const invalidDirectives = [];
    let format;
    let decimalPlaces;
    const rows = source.split("\n").map((row, lineIndex) => {
      const result = parseFormatDirectiveLine(row, lineIndex);
      if (result.status === "not-directive") return row;
      directiveLineIndexes.push(lineIndex);
      if (result.status === "invalid") {
        invalidDirectives.push(result.error);
      } else if (result.directive.kind === "format") {
        format = result.directive.value;
      } else {
        decimalPlaces = result.directive.value;
      }
      return row.endsWith("\r") ? "\r" : "";
    });
    return { format, decimalPlaces, directiveLineIndexes, invalidDirectives, sourceWithoutDirectives: rows.join("\n") };
  }

  function preProcessBlock(source) {
    const rawRows = source.split("\n");
    const directives = collectFormatDirectives(source);
    let processed = directives.sourceWithoutDirectives;
    const emitterLines = [];
    const insertionLines = [];
    const hiddenLines = directives.directiveLineIndexes.slice();
    let hideNonEmitters = false;
    for (let i = 0; i < rawRows.length; i++) {
      if (/^[^#\r\n]*=>.*$/.test(rawRows[i])) emitterLines.push(i);
      if (/@\s*\[([^\]:]+)(::)?([^\]]*)\].*$/.test(rawRows[i])) insertionLines.push(i);
      if (/^\s*@hideRows\s*$/.test(rawRows[i])) {
        hiddenLines.push(i);
        hideNonEmitters = true;
      }
      if (/^\s*@createUnit\s*$/.test(rawRows[i])) hiddenLines.push(i);
    }
    processed = processed.replace(/^([^#\r\n]*?)([\t ]*=>[\t ]*)(\$\{.*\})?(.*)$/gm, "$1");
    processed = processed.replace(/@\s*\[([^\]:]+)(::[^\]]*)?\](.*)$/gm, "$1$3");
    processed = processed.replace(/@sum/gi, "__total");
    processed = processed.replace(/@total/gi, "__total");
    processed = processed.replace(/@prev/gi, "__prev");
    processed = processed.replace(/^\s*@hideRows/gim, "");
    processed = replaceStrings(processed, preProcessors);
    return {
      rawRows,
      processed,
      transparentLines: directives.directiveLineIndexes,
      overrides: { numberFormat: directives.format, decimalPlaces: directives.decimalPlaces },
      invalidDirectives: directives.invalidDirectives,
      emitterLines,
      insertionLines,
      hiddenLines,
      hideNonEmitters,
    };
  }

  // ---- scope ----

  function scopeFromMetadata(metadata, forceAll) {
    const scope = new Map();
    const warnings = [];
    if (!metadata || typeof metadata !== "object") return { scope, warnings };
    let selected = {};
    const has = (key) => Object.prototype.hasOwnProperty.call(metadata, key);
    if (has("numerals")) {
      const setting = metadata.numerals;
      if (setting === "all") {
        for (const [key, value] of Object.entries(metadata)) if (key !== "numerals") selected[key] = value;
      } else if (typeof setting === "string" && setting !== "none") {
        if (has(setting)) selected[setting] = metadata[setting];
      } else if (Array.isArray(setting)) {
        for (const entry of setting) {
          const key = String(entry);
          if (has(key)) selected[key] = metadata[key];
        }
      }
    } else if (forceAll) {
      selected = Object.assign({}, metadata);
    }
    for (const [key, value] of Object.entries(metadata)) if (key.startsWith("$")) selected[key] = value;

    for (const [key, raw] of Object.entries(selected)) {
      let value = math.isUnit(raw) ? raw.valueOf() : raw;
      if (Array.isArray(value)) value = value[value.length - 1];
      if (typeof value === "number") {
        scope.set(key, math.number(value));
      } else if (typeof value === "string") {
        const processed = replaceStrings(value, preProcessors);
        const functionMatch = key.match(/^([^(]+)\(([^)]*)\)$/);
        if (functionMatch) {
          try {
            scope.set(functionMatch[1], math.evaluate(`${functionMatch[1]}(${functionMatch[2]}) = ${processed}`, scope));
          } catch (error) {
            warnings.push(`Frontmatter: error evaluating function "${key}": ${messageOf(error)}`);
          }
        } else {
          let evaluated;
          try {
            evaluated = math.evaluate(processed, scope);
          } catch (error) {
            warnings.push(`Frontmatter: error evaluating "${key}": ${messageOf(error)}`);
          }
          if (evaluated !== undefined) scope.set(key, evaluated);
        }
      } else if (typeof value === "function") {
        scope.set(key, value);
      } else if (value !== null && typeof value === "object") {
        warnings.push(`Frontmatter: value for "${key}" is an object and will be ignored. ` +
          `Consider surrounding the value with quotes (e.g. \`${key}: "value"\`).`);
      }
    }
    return { scope, warnings };
  }

  function currentMetadata() {
    const metadata = Object.assign({}, noteMetadata);
    for (const [key, value] of noteGlobals) metadata[key] = value;
    return metadata;
  }

  function addGlobals(scope) {
    for (const [key, value] of scope) if (key.startsWith("$")) noteGlobals.set(key, value);
  }

  // ---- cross-note references ([[Note]].property) ----

  const CROSS_NOTE_REF_REGEX = /\[\[([^\]]+)\]\]\.([$\wÀ-ʯͰ-Ͽ℀-⅏]+(?:\.[$\wÀ-ʯͰ-Ͽ℀-⅏]+)*)/g;

  function availableProperties(metadata) {
    const result = {};
    const setting = metadata.numerals;
    if (setting === "none") {
      // nothing but $-globals
    } else if (setting === "all" || (setting === undefined && settings.force_all)) {
      for (const [key, value] of Object.entries(metadata)) if (key !== "numerals") result[key] = value;
    } else if (typeof setting === "string") {
      if (Object.prototype.hasOwnProperty.call(metadata, setting)) result[setting] = metadata[setting];
    } else if (Array.isArray(setting)) {
      for (const entry of setting) {
        const key = String(entry);
        if (Object.prototype.hasOwnProperty.call(metadata, key)) result[key] = metadata[key];
      }
    }
    for (const [key, value] of Object.entries(metadata)) if (key.startsWith("$")) result[key] = value;
    return result;
  }

  function evaluateMetadataValue(value) {
    if (Array.isArray(value)) value = value[value.length - 1];
    if (value === undefined || value === null) return { error: "Value is undefined" };
    if (typeof value === "number") return { result: math.number(value) };
    if (typeof value === "object") return { result: value };
    if (typeof value === "string") {
      try {
        return { result: math.evaluate(replaceStrings(value, preProcessors)) };
      } catch (error) {
        return { error: messageOf(error) };
      }
    }
    return { result: value };
  }

  function formatValueForInsertion(value) {
    if (typeof value === "number") return String(value);
    if (typeof value === "string") return value;
    try {
      return math.format(value, { notation: "fixed" });
    } catch (_) {
      return String(value);
    }
  }

  function resolveReference(noteName, propertyPath) {
    const target = references[noteName];
    if (!target) return { error: `Note "${noteName}" not found in vault` };
    const available = availableProperties(target.metadata || {});
    const parts = propertyPath.split(".");
    const top = parts[0];
    if (!(top in available)) {
      return {
        error: `Property "${top}" not available in "${noteName}". ` +
          "Ensure the property exists and is exposed via the `numerals` frontmatter key or starts with `$`.",
      };
    }
    let raw = available[top];
    if (parts.length > 1) {
      if (typeof raw !== "object" || raw === null || Array.isArray(raw)) {
        return { error: `Property "${top}" in "${noteName}" is not an object; cannot access "${propertyPath}"` };
      }
      for (const part of parts.slice(1)) raw = raw === null || typeof raw !== "object" ? undefined : raw[part];
      if (raw === undefined) return { error: `Property "${propertyPath}" not found in "${noteName}"` };
    } else {
      const { scope } = scopeFromMetadata(available, true);
      if (scope.has(top)) return { value: formatValueForInsertion(scope.get(top)) };
    }
    const { result, error } = evaluateMetadataValue(raw);
    if (result === undefined || error) {
      return { warning: `Could not evaluate "${propertyPath}" from "${noteName}": ${error || "undefined value"}` };
    }
    return { value: formatValueForInsertion(result) };
  }

  function resolveCrossNoteReferences(source) {
    const empty = { resolved: source, warnings: [], error: null };
    if (!settings.cross_note_references) return empty;
    const warnings = [];
    let resolved = source;
    CROSS_NOTE_REF_REGEX.lastIndex = 0;
    const matches = [];
    let match;
    while ((match = CROSS_NOTE_REF_REGEX.exec(source)) !== null) matches.push(match);
    for (const found of matches) {
      const result = resolveReference(found[1], found[2]);
      if (result.error) return { resolved, warnings, error: result.error };
      if (result.warning) {
        warnings.push(result.warning);
        continue;
      }
      resolved = resolved.split(found[0]).join(`(${result.value})`);
    }
    return { resolved, warnings, error: null };
  }

  // ---- evaluation ----

  function evaluateRows(processed, scope, transparentLines) {
    let errorMsg = null;
    let errorInput = "";
    const rows = processed.split("\n");
    const results = [];
    const inputs = [];
    const transparent = new Set(transparentLines);
    const segment = [];
    let hasPrevious = false;
    let previous;
    const toProcess = rows[rows.length - 1] === "" ? rows.slice(0, -1) : rows;
    for (const [index, row] of toProcess.entries()) {
      if (transparent.has(index)) {
        results.push(undefined);
        inputs.push(row);
        continue;
      }
      try {
        if (hasPrevious) {
          scope.set("__prev", previous);
        } else {
          scope.set("__prev", undefined);
          if (/__prev/i.test(row)) {
            errorMsg = { name: "Previous Value Error", message: "Error evaluating @prev directive. There is no previous result." };
            errorInput = row;
            break;
          }
        }
        if (segment.length > 1) {
          try {
            scope.set("__total", math.add.apply(math, segment));
          } catch (_) {
            scope.set("__total", undefined);
            if (/__total/i.test(row)) {
              errorMsg = { name: "Summing Error", message: "Error evaluating @sum or @total directive. Previous lines may not be summable." };
              errorInput = row;
              break;
            }
          }
        } else if (segment.length === 1) {
          scope.set("__total", segment[0]);
        } else {
          scope.set("__total", undefined);
        }
        const result = math.evaluate(row, scope);
        results.push(result);
        inputs.push(row);
        hasPrevious = true;
        previous = result;
        if (result === undefined) segment.length = 0;
        else segment.push(result);
      } catch (error) {
        errorMsg = { name: (error && error.name) || "Error", message: messageOf(error) };
        errorInput = row;
        break;
      }
    }
    return { results, inputs, errorMsg, errorInput };
  }

  // ---- formatting ----

  function groupInteger(digits, locale) {
    if (!locale.indian) return digits.replace(/\B(?=(\d{3})+(?!\d))/g, locale.group);
    if (digits.length <= 3) return digits;
    const last = digits.slice(-3);
    const rest = digits.slice(0, -3).replace(/\B(?=(\d{2})+(?!\d))/g, locale.group);
    return rest + locale.group + last;
  }

  // Intl.NumberFormat's decimal style: grouping, minFrac..maxFrac digits.
  function localeNumber(value, locale, minFrac, maxFrac, grouping) {
    if (!Number.isFinite(value)) return value > 0 ? "∞" : value < 0 ? "-∞" : "NaN";
    const negative = value < 0 || Object.is(value, -0);
    let fixed = Math.abs(value).toFixed(maxFrac);
    if (/e/.test(fixed)) fixed = Math.abs(value).toLocaleString("en-US", { useGrouping: false });
    let [integer, fraction = ""] = fixed.split(".");
    while (fraction.length > minFrac && fraction.endsWith("0")) fraction = fraction.slice(0, -1);
    const groupedInteger = grouping === false ? integer : groupInteger(integer, locale);
    const text = fraction ? groupedInteger + locale.decimal + fraction : groupedInteger;
    const isZero = /^[0.,\s ]*$/.test(text.replace(locale.group, ""));
    return negative && !isZero ? "-" + text : text;
  }

  function significant(value, digits) {
    return Number(value.toPrecision(digits));
  }

  function countLeadingDecimalZeroes(value) {
    const abs = Math.abs(value);
    if (abs >= 1 || abs === 0) return 0;
    return Math.max(0, -Math.floor(Math.log10(abs)) - 1);
  }

  // getLocaleFormatter: three fraction digits, falling back to 15
  // significant digits (or exponential) when a nonzero value would show as 0.
  function localeFormatter(locale, grouping) {
    return (value) => {
      if (typeof value !== "number") return math.format(value);
      const text = localeNumber(value, locale, 0, 3, grouping);
      if (Number.isFinite(value) && value !== 0 && /^-?0$/.test(text)) {
        if (countLeadingDecimalZeroes(value) > MAX_FIXED_FORMAT_LEADING_DECIMAL_ZEROES) {
          return math.format(value, { notation: "exponential" });
        }
        const precise = significant(value, 15);
        const places = Math.min(20, Math.max(0, countLeadingDecimalZeroes(value) + 15));
        return localeNumber(precise, locale, 0, places, grouping);
      }
      return text;
    };
  }

  function profileFor(format) {
    switch (format) {
      case "fixed": return { notation: "fixed", mathjs: { notation: "fixed" } };
      case "exponential": return { notation: "exponential", mathjs: { notation: "exponential" } };
      case "engineering": return { notation: "engineering", mathjs: { notation: "engineering" } };
      default: {
        const locale = locales[format] || locales.system;
        return { notation: "standard", locale, mathjs: localeFormatter(locale, true) };
      }
    }
  }

  function decompose(value) {
    const [coefficient, exponent] = value.toExponential().split("e");
    return { coefficient: Number(coefficient), exponent: Number(exponent) };
  }

  function roundedFixed(value, places) {
    return math.format(value, { notation: "fixed", precision: places });
  }

  function formatExponential(value, places) {
    if (value === 0) return `${roundedFixed(value, places)}e+0`;
    let { coefficient, exponent } = decompose(value);
    let text = roundedFixed(coefficient, places);
    if (Math.abs(Number(text)) >= 10) {
      exponent += 1;
      coefficient /= 10;
      text = roundedFixed(coefficient, places);
    }
    return `${text}e${exponent >= 0 ? "+" : ""}${exponent}`;
  }

  function formatEngineering(value, places) {
    if (value === 0) return `${roundedFixed(value, places)}e+0`;
    const scientific = decompose(value);
    let exponent = Math.floor(scientific.exponent / 3) * 3;
    let coefficient = scientific.coefficient * Math.pow(10, scientific.exponent - exponent);
    let text = roundedFixed(coefficient, places);
    if (Math.abs(Number(text)) >= 1000) {
      exponent += 3;
      coefficient /= 1000;
      text = roundedFixed(coefficient, places);
    }
    return `${text}e${exponent >= 0 ? "+" : ""}${exponent}`;
  }

  function overrideFormatter(profile, places) {
    return (value) => {
      if (math.isFraction(value)) value = Number(value.valueOf());
      if (math.isBigNumber(value)) value = value.toNumber();
      if (typeof value !== "number") {
        return math.format(value, { notation: profile.notation === "standard" ? "fixed" : profile.notation, precision: places });
      }
      if (!Number.isFinite(value)) return math.format(value);
      switch (profile.notation) {
        case "exponential": return formatExponential(value, places);
        case "engineering": return formatEngineering(value, places);
        case "fixed": return roundedFixed(value, places);
        default: return localeNumber(value, profile.locale, places, places, true);
      }
    };
  }

  function formatWithProfile(value, profile, places) {
    if (places === undefined) return math.format(value, profile.mathjs);
    if (math.isFraction(value)) return overrideFormatter(profile, places)(Number(value.valueOf()));
    return math.format(value, overrideFormatter(profile, places));
  }

  function matchCurrency(value) {
    if (!math.isUnit(value)) return undefined;
    for (const definition of currencies) {
      if (!value.equalBase(definition.unit)) continue;
      try {
        return { definition, amount: value.toNumber(definition.code) };
      } catch (_) {
        return undefined;
      }
    }
    return undefined;
  }

  function texCurrencyReplacement(tex) {
    for (const entry of defaultCurrencyMap) {
      tex = tex.replace(new RegExp("\\\\*\\" + entry.symbol, "g"), "\\" + entry.name + " ");
    }
    return tex;
  }

  function escapeTexText(text) {
    return String(text).replace(/([\\{}#$%&_^~])/g, "\\$1");
  }

  function resultToTeX(value) {
    try {
      let text = math.format(value, localeFormatter(locales.system, false));
      text = replaceStrings(text, preProcessors);
      return texCurrencyReplacement(math.parse(text).toTex());
    } catch (_) {
      if (value === Infinity) return "\\infty";
      if (value === -Infinity) return "-\\infty";
      if (typeof value === "number" && Number.isNaN(value)) return "\\mathrm{NaN}";
      return `\\text{${escapeTexText(math.format(value))}}`;
    }
  }

  function format(value, overrides) {
    const profile = profileFor(overrides.numberFormat || settings.number_format);
    const currency = matchCurrency(value);
    const usesCurrency = currency && (overrides.decimalPlaces !== undefined ||
      settings.currency_precision === "currency-standard" || settings.currency_display === "symbol");
    if (usesCurrency) {
      const places = overrides.decimalPlaces !== undefined
        ? overrides.decimalPlaces
        : (settings.currency_precision === "currency-standard" ? currency.definition.fractionDigits : undefined);
      const amount = (number) => (places === undefined ? formatWithProfile(number, profile) : overrideFormatter(profile, places)(number));
      let text;
      if (settings.currency_display === "symbol") {
        const negative = currency.amount < 0 || Object.is(currency.amount, -0);
        text = (negative ? "-" : "") + currency.definition.symbol + amount(Math.abs(currency.amount));
      } else {
        text = `${amount(currency.amount)} ${currency.definition.code}`;
      }
      const plain = profileFor("fixed");
      const canonicalNumber = places === undefined
        ? (profile.notation === "standard" ? math.format(currency.amount, localeFormatter(locales.system, false)) : formatWithProfile(currency.amount, profile))
        : (profile.notation === "standard" ? overrideFormatter(plain, places)(currency.amount) : overrideFormatter(profile, places)(currency.amount));
      const tex = settings.currency_display === "symbol"
        ? `${currency.amount < 0 ? "-" : ""}\\${currency.definition.name} ${canonicalNumber.replace(/^-/, "")}`
        : `${canonicalNumber}~\\mathrm{${escapeTexText(currency.definition.code)}}`;
      return { text, tex, canonical: `${canonicalNumber} ${currency.definition.code}` };
    }
    const text = formatWithProfile(value, profile, overrides.decimalPlaces);
    const tex = overrides.decimalPlaces === undefined && overrides.numberFormat === undefined
      ? resultToTeX(value)
      : `\\text{${escapeTexText(text)}}`;
    let canonical = text;
    if (currency) canonical = `${formatWithProfile(currency.amount, profile)} ${currency.definition.code}`;
    return { text, tex, canonical };
  }

  const subscriptRegex = /([\p{L}\p{Nl}_$])([\p{L}\p{Nl}_$À-ʯͰ-Ͽ℀-⅏\u{1D400}-\u{1D7FF}\d]*)(\\_)([\p{L}\p{Nl}_$À-ʯͰ-Ͽ℀-⅏\u{1D400}-\u{1D7FF}\d]+)/gu;

  function expressionToTeX(processed, raw) {
    let tex = math.parse(processed).toTex();
    const directives = raw.match(/@(sum|total)\b/g);
    tex = tex.replace(/(__total|\\_\\_total)\b/g, () => (directives && directives.shift()) || "@Sum");
    tex = tex.replace(/(\\_\\_|__)prev\b/g, "@prev");
    tex = tex.replace(subscriptRegex, (_, start, body, __, end) => `${start}${body}_{${end}}`);
    return texCurrencyReplacement(tex);
  }

  function cleanRawInput(raw) {
    let cleaned = raw;
    if (settings.hide_emitter_markup) cleaned = cleaned.replace(/^([^#\r\n]*?)([\t ]*=>[\t ]*)(\$\{.*\})?(.*)$/gm, "$1$4");
    return cleaned.replace(/@\s*\[([^\]:]+)(::[^\]]*)?\](.*)$/gm, "$1$3");
  }

  function messageOf(error) {
    return error && error.message !== undefined ? String(error.message) : String(error);
  }

  // ---- entry points ----

  function block(source, style) {
    const output = { lines: [], error: null, warnings: [], insertions: [], emitters: false };
    const crossNote = resolveCrossNoteReferences(source);
    if (crossNote.error) {
      output.error = {
        name: "Note Reference Error",
        message: crossNote.error,
        input: source.split("\n").find((line) => line.includes("[[") && line.includes("]].")) || source.split("\n")[0] || "",
      };
      return JSON.stringify(output);
    }
    const processed = preProcessBlock(crossNote.resolved);
    if (processed.invalidDirectives.length > 0) {
      const directive = processed.invalidDirectives[0];
      output.error = { name: "Formatting Directive Error", message: directive.message, input: directive.source };
      return JSON.stringify(output);
    }
    output.emitters = processed.emitterLines.length > 0;
    const { scope, warnings } = scopeFromMetadata(currentMetadata(), settings.force_all);
    const evaluation = evaluateRows(processed.processed, scope, processed.transparentLines);
    addGlobals(scope);
    for (const index of processed.insertionLines) {
      if (index >= evaluation.results.length || evaluation.results[index] === undefined) continue;
      output.insertions.push({ line: index, value: format(evaluation.results[index], processed.overrides).canonical });
    }
    for (let index = 0; index < evaluation.inputs.length; index++) {
      const isEmitter = processed.emitterLines.includes(index);
      const hidden = processed.hiddenLines.includes(index) ||
        (processed.hideNonEmitters && !isEmitter) ||
        (output.emitters && settings.hide_non_emitters && !isEmitter);
      if (hidden) continue;
      const cleaned = cleanRawInput(processed.rawRows[index] || "");
      const commentMatch = cleaned.match(/#.+$/);
      const input = commentMatch ? cleaned.replace(/#.+$/, "") : cleaned;
      const result = evaluation.results[index];
      const line = { line: index, input: input.replace(/\s+$/, ""), comment: commentMatch ? commentMatch[0] : null, emitter: isEmitter, result: null };
      if (result !== undefined) {
        const formatted = format(result, processed.overrides);
        line.result = formatted.text;
        if (style === "tex") {
          try {
            line.tex_input = expressionToTeX(evaluation.inputs[index], processed.rawRows[index] || "");
          } catch (_) {
            line.tex_input = `\\text{${escapeTexText(input)}}`;
          }
          line.tex_result = formatted.tex;
        }
      }
      output.lines.push(line);
    }
    if (evaluation.errorMsg) {
      output.error = { name: evaluation.errorMsg.name, message: evaluation.errorMsg.message, input: evaluation.errorInput };
    }
    output.warnings = crossNote.warnings.concat(warnings);
    return JSON.stringify(output);
  }

  // `mode` is "result" or "equation"; `style` is "plain" or "tex".
  // `continuesSection` keeps @prev from the previous inline expression.
  function inline(expression, mode, style, continuesSection) {
    if (!continuesSection) prevInline = undefined;
    const crossNote = resolveCrossNoteReferences(expression);
    if (crossNote.error) throw new Error(crossNote.error);
    let processed = replaceStrings(crossNote.resolved, preProcessors);
    processed = processed.replace(/@prev/gi, "__prev");
    const { scope } = scopeFromMetadata(currentMetadata(), settings.force_all);
    const before = new Map(scope);
    if (/__prev/i.test(processed)) {
      if (prevInline === undefined) {
        prevInline = undefined;
        throw new Error("Error evaluating @prev directive. There is no previous inline result.");
      }
      scope.set("__prev", prevInline);
    }
    let result;
    try {
      result = math.evaluate(processed, scope);
    } catch (error) {
      prevInline = undefined;
      throw error;
    }
    if (result === undefined) {
      prevInline = undefined;
      throw new Error("Expression produced no result");
    }
    prevInline = result;
    for (const [key, value] of scope) {
      if (key.startsWith("$") && value !== before.get(key)) noteGlobals.set(key, value);
    }
    const formatted = format(result, {});
    if (style === "tex") {
      if (mode === "equation") {
        let left;
        try {
          left = expressionToTeX(processed, expression);
        } catch (_) {
          left = `\\text{${escapeTexText(expression)}}`;
        }
        return JSON.stringify({ text: `${left} = ${formatted.tex}`, tex: true });
      }
      return JSON.stringify({ text: formatted.tex, tex: true });
    }
    if (mode === "equation") {
      return JSON.stringify({ text: `${expression}${settings.equation_separator}${formatted.text}`, tex: false });
    }
    return JSON.stringify({ text: formatted.text, tex: false });
  }

  function pristine() {
    return changed ? "false" : "true";
  }

  globalThis.__numerals = { configure, beginNote, block, inline, pristine };
})();
