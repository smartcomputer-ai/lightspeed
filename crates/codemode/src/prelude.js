(function (send, emit, catalogJson) {
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
    const catalog = parse(catalogJson);
    for (let i = 0; i < catalog.length; i++) {
        const binding = catalog[i];
        const id = binding.binding_id;
        const invoke = async function (args) {
            const json = serialize(args);
            return await new NativePromise(function (resolve, reject) {
                const request = send(id, json);
                pending[request] = { resolve, reject };
            });
        };
        define(tools, binding.name, { value: freeze(invoke), enumerable: true });
    }
    freeze(tools);
    const text = freeze(function (value) { emit(serialize(value)); });
    return {
        compile: function (source) {
            const body = new AsyncFunction("tools", "text", "\"use strict\";\n" + source);
            return function () { return body(tools, text); };
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
