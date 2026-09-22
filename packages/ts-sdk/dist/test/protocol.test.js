"use strict";
var __importDefault = (this && this.__importDefault) || function (mod) {
    return (mod && mod.__esModule) ? mod : { "default": mod };
};
Object.defineProperty(exports, "__esModule", { value: true });
const node_test_1 = require("node:test");
const strict_1 = __importDefault(require("node:assert/strict"));
const node_stream_1 = require("node:stream");
const protocol_js_1 = require("../src/protocol.js");
(0, node_test_1.describe)("decodeLine", () => {
    (0, node_test_1.it)("parses a normal line", () => {
        strict_1.default.deepEqual((0, protocol_js_1.decodeLine)('{"jsonrpc":"2.0","id":1}'), {
            jsonrpc: "2.0",
            id: 1,
        });
    });
    (0, node_test_1.it)("skips blank and whitespace-only lines", () => {
        strict_1.default.equal((0, protocol_js_1.decodeLine)(""), null);
        strict_1.default.equal((0, protocol_js_1.decodeLine)("   \n"), null);
    });
    (0, node_test_1.it)("throws ProtocolError on malformed json", () => {
        strict_1.default.throws(() => (0, protocol_js_1.decodeLine)("{ not json"), protocol_js_1.ProtocolError);
    });
    (0, node_test_1.it)("throws ProtocolError on non-object payloads", () => {
        strict_1.default.throws(() => (0, protocol_js_1.decodeLine)("[1,2]"), protocol_js_1.ProtocolError);
        strict_1.default.throws(() => (0, protocol_js_1.decodeLine)("42"), protocol_js_1.ProtocolError);
    });
});
(0, node_test_1.describe)("builders", () => {
    (0, node_test_1.it)("buildResult", () => {
        strict_1.default.deepEqual((0, protocol_js_1.buildResult)(1, { ok: true }), {
            jsonrpc: "2.0",
            id: 1,
            result: { ok: true },
        });
    });
    (0, node_test_1.it)("buildError omits data when absent", () => {
        strict_1.default.deepEqual((0, protocol_js_1.buildError)(1, -32601, "nope"), {
            jsonrpc: "2.0",
            id: 1,
            error: { code: -32601, message: "nope" },
        });
        const withData = (0, protocol_js_1.buildError)(1, -32603, "boom", { x: 1 });
        strict_1.default.deepEqual(withData["error"]["data"], { x: 1 });
    });
    (0, node_test_1.it)("buildRequest / buildNotification", () => {
        strict_1.default.deepEqual((0, protocol_js_1.buildRequest)("m", { a: 1 }, "p1"), {
            jsonrpc: "2.0",
            id: "p1",
            method: "m",
            params: { a: 1 },
        });
        strict_1.default.deepEqual((0, protocol_js_1.buildNotification)("m", null), {
            jsonrpc: "2.0",
            method: "m",
            params: null,
        });
    });
    (0, node_test_1.it)("error codes match JSON-RPC", () => {
        strict_1.default.equal(protocol_js_1.ERR_PARSE, -32700);
        strict_1.default.equal(protocol_js_1.ERR_INVALID_REQUEST, -32600);
        strict_1.default.equal(protocol_js_1.ERR_METHOD_NOT_FOUND, -32601);
        strict_1.default.equal(protocol_js_1.ERR_INVALID_PARAMS, -32602);
        strict_1.default.equal(protocol_js_1.ERR_INTERNAL, -32603);
    });
});
(0, node_test_1.describe)("writeMessage", () => {
    (0, node_test_1.it)("writes one JSON line", () => {
        const out = new node_stream_1.PassThrough();
        let buf = "";
        out.on("data", (c) => {
            buf += String(c);
        });
        (0, protocol_js_1.writeMessage)({ a: 1 }, out);
        strict_1.default.equal(buf, '{"a":1}\n');
    });
});
