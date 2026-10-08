(function (send, emit, select, catalogJson) {
    "use strict";
    // Capture intrinsics before user code can replace globals or prototypes.
    const parse = JSON.parse;
    const stringify = JSON.stringify;
    const keys = Object.keys;
    const prototype = Object.getPrototypeOf;
    const descriptor = Object.getOwnPropertyDescriptor;
    const hasOwn = Object.hasOwn;
    const symbols = Object.getOwnPropertySymbols;
    const create = Object.create;
    const freeze = Object.freeze;
    const define = Object.defineProperty;
    const isArray = Array.isArray;
    const isFinite = Number.isFinite;
    const PlainObject = Object.prototype;
    const NativePromise = Promise;
    const NativeError = Error;
    const NativeTypeError = TypeError;
    const AsyncFunction = (async function () {}).constructor;
    const pending = create(null);
    const tools = create(null);
    const bindingIds = create(null);

    // Clone only JSON data, rejecting silent JSON.stringify losses such as
    // undefined fields, functions, non-finite numbers, accessors and array holes.
    // Null-prototype copies prevent a user-supplied toJSON hook from running.
    function clone(value, ancestors) {
        if (value === null || typeof value === "string" || typeof value === "boolean") return value;
        if (typeof value === "number" && isFinite(value)) return value;
        if (typeof value !== "object") throw new NativeTypeError("Value is not JSON-compatible");
        if (ancestors.length >= 128) throw new NativeTypeError("JSON nesting is too deep");
        for (let i = 0; i < ancestors.length; i++) {
            if (ancestors[i] === value) throw new NativeTypeError("Cyclic value is not JSON-compatible");
        }
        const array = isArray(value);
        const proto = prototype(value);
        if (!array && proto !== null && proto !== PlainObject) {
            throw new NativeTypeError("Only plain objects and arrays are JSON-compatible");
        }
        if (symbols(value).length !== 0) throw new NativeTypeError("Symbol keys are not JSON-compatible");
        const output = array ? [] : create(null);
        // Also mask inherited toJSON on arrays without affecting JSON elements.
        if (array) define(output, "toJSON", { value: undefined });
        const names = keys(value);
        if (array && names.length !== value.length) {
            throw new NativeTypeError("Sparse arrays and extra array properties are not JSON-compatible");
        }
        ancestors[ancestors.length] = value;
        for (let i = 0; i < names.length; i++) {
            const key = names[i];
            if (array && key !== "" + i) throw new NativeTypeError("Invalid array property");
            const field = descriptor(value, key);
            if (!field || !hasOwn(field, "value")) throw new NativeTypeError("Accessors are not JSON-compatible");
            define(output, key, { value: clone(field.value, ancestors), enumerable: true, configurable: true, writable: true });
        }
        ancestors.length--;
        return output;
    }
    function serialize(value) {
        const ancestors = create(null);
        ancestors.length = 0;
        return stringify(clone(value, ancestors));
    }
    function copy(value) {
        const ancestors = create(null);
        ancestors.length = 0;
        return clone(value, ancestors);
    }
    function invoke(id, args, selection) {
        const json = serialize(args);
        let request;
        const promise = new NativePromise(function (resolve, reject) {
            request = send(id, json, selection);
            pending[request] = { resolve, reject };
        });
        return { request, promise };
    }
    const catalog = parse(catalogJson);
    for (let i = 0; i < catalog.length; i++) {
        const binding = catalog[i];
        const id = binding.binding_id;
        bindingIds[binding.name] = id;
        const tool = async function (args) {
            return await invoke(id, args, "").promise;
        };
        define(tools, binding.name, { value: freeze(tool), enumerable: true });
    }
    freeze(tools);
    const text = freeze(function (value) { emit(serialize(value)); });
    function requiredBinding(name) {
        if (!hasOwn(bindingIds, name)) {
            const error = new NativeError("Output helper requires the admitted " + name + " tool");
            define(error, "kind", { value: "unsupported_capability", enumerable: true });
            throw error;
        }
        return bindingIds[name];
    }
    function outputHelper(kind) {
        return freeze(async function (value, options) {
            const source = copy(value);
            const metadata = options === undefined ? create(null) : copy(options);
            if (metadata === null || typeof metadata !== "object" || isArray(metadata)) {
                throw new NativeTypeError("Output options must be an object with optional name and media_type");
            }
            const optionKeys = keys(metadata);
            for (let i = 0; i < optionKeys.length; i++) {
                const key = optionKeys[i];
                if (key !== "name" && key !== "media_type") throw new NativeTypeError("Unknown output option: " + key);
                if (typeof metadata[key] !== "string") throw new NativeTypeError("Output option " + key + " must be a string");
            }
            let inline = false;
            if (typeof source !== "string") {
                if (source === null || typeof source !== "object" || isArray(source)) {
                    throw new NativeTypeError("Output requires a content reference, descriptor, or explicit text/json/bytes source");
                }
                if (!hasOwn(source, "content_ref") && !hasOwn(source, "blobRef")) {
                    let representations = 0;
                    const sourceKeys = keys(source);
                    for (let i = 0; i < sourceKeys.length; i++) {
                        const key = sourceKeys[i];
                        if (key === "text" || key === "json" || key === "bytes") representations++;
                        else if (key !== "name" && key !== "media_type") throw new NativeTypeError("Unknown inline content field: " + key);
                    }
                    if (representations !== 1) throw new NativeTypeError("Inline output requires exactly one of text, json, or bytes");
                    inline = true;
                }
            }
            // Check every required capability before storing an inline source.
            const admission = requiredBinding(kind === "media" ? "blob_read" : "blob_info");
            const put = inline ? requiredBinding("blob_put") : undefined;
            let reference = source;
            if (inline) {
                for (let i = 0; i < optionKeys.length; i++) source[optionKeys[i]] = metadata[optionKeys[i]];
                reference = await invoke(put, source, "").promise;
            }
            if (optionKeys.length !== 0) {
                if (typeof reference === "string") {
                    const object = create(null);
                    object.content_ref = reference;
                    reference = object;
                } else {
                    reference = copy(reference);
                }
                if (hasOwn(metadata, "name")) reference.name = metadata.name;
                if (hasOwn(metadata, "media_type")) {
                    delete reference.mediaType;
                    delete reference.mimeType;
                    reference.media_type = metadata.media_type;
                }
            }
            const args = create(null);
            args.ref = reference;
            if (kind === "media") args.format = "media";
            else {
                args.presentation = "file";
                if (hasOwn(metadata, "name")) args.name = metadata.name;
            }
            const call = invoke(admission, args, kind);
            const admitted = await call.promise;
            // Only this closure has the native emitter. It authenticates this
            // successful admission by the native request identity, never by
            // guest-editable descriptor fields or a value supplied to text().
            select(call.request);
            return admitted;
        });
    }
    const media = outputHelper("media");
    const file = outputHelper("file");
    return {
        compile: function (source) {
            // A lexical block lets existing scripts keep local names such as
            // `const file` without redeclaring the helper parameters. Parse the
            // entire block before invoking it, as for every authored script.
            const body = new AsyncFunction("tools", "text", "media", "file", "\"use strict\";\n{\n" + source + "\n}");
            return function () { return body(tools, text, media, file); };
        },
        serialize,
        deliver: function (id, success, json) {
            const pair = pending[id];
            if (!pair) throw new NativeError("Unknown host completion");
            delete pending[id];
            const value = parse(json);
            if (success) pair.resolve(value);
            else {
                const error = new NativeError(value.message);
                define(error, "kind", { value: value.kind, enumerable: true });
                define(error, "value", { value: value.value, enumerable: true });
                pair.reject(error);
            }
        }
    };
})
