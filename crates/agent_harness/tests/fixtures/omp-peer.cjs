const assert = require("node:assert/strict");
const readline = require("node:readline");
const args = process.argv.slice(2);
const scenario = args[0];
const models = [
  {provider: "native", id: "first", name: "First model", reasoning: false},
  {provider: "native", id: "answer/model", name: "Answer model", reasoning: true},
];
let model = models[0];
let thinking = "medium";
let negotiated = false;
let filtered = false;
let askEnabled = false;
let prompt;
function emit(frame) { console.log(JSON.stringify(frame)); }
function delta(type, text) {
  emit({type: "message_update", messageId: "msg-2", message: {role: "assistant"},
    assistantMessageEvent: {type, contentIndex: 0, delta: text}});
}
function result(status, settled = true) {
  emit({type: "prompt_result", id: prompt.id, agentInvoked: true, status,
    sessionSettled: settled, ...(status === "error" ? {error: {message: "Provider rejected credentials", provider: "native", model: model.id, httpStatus: 401, retryable: false}} : {})});
}
emit({type: "ready", protocolVersion: 1, supportedProtocolVersions: [1, 2], maxFrameBytes: 1048576, maxReassembledFrameBytes: 67108864});
readline.createInterface({input: process.stdin}).on("line", line => {
  const command = JSON.parse(line);
  assert.equal(typeof command.id, "string", "RPC ids are strings");
  const response = data => emit({type: "response", id: command.id, command: command.type, success: true, data});
  switch (command.type) {
    case "negotiate_protocol":
      assert.equal(command.protocolVersion, 2);
      negotiated = true;
      response({protocolVersion: 2});
      // Startup quiescence must never finish the host's yet-unsent prompt.
      emit({type: "session_settled"});
      break;
    case "set_event_filter":
      assert(negotiated);
      assert.equal(command.messageUpdates, "delta");
      filtered = true;
      response({events: null, messageUpdates: "delta"});
      break;
    case "set_ask_dialog":
      askEnabled = command.enabled;
      response({});
      break;
    case "get_available_models":
      assert(negotiated);
      response({models});
      break;
    case "set_model":
      model = models.find(model => model.provider === command.provider && model.id === command.modelId);
      assert(model, "requested model exists");
      response(model);
      break;
    case "set_thinking_level":
      thinking = command.level;
      response({});
      break;
    case "get_state":
      response({model, thinkingLevel: thinking, sessionFile: "saved-session.jsonl", sessionId: "session-1", isSettled: true, hasPendingAsyncWork: false});
      break;
    case "prompt":
      assert(filtered && askEnabled);
      prompt = command;
      if (scenario === "selected") {
        assert.equal(model.id, "answer/model");
        assert.equal(thinking, "high");
        assert.equal(args[args.indexOf("--session") + 1], "resume-session.jsonl");
        assert.equal(args[args.indexOf("--approval-mode") + 1], "yolo");
      }
      if (scenario === "command-error") {
        emit({type: "response", id: command.id, command: "prompt", success: false, error: "Prompt rejected"});
        break;
      }
      if (scenario === "local") {
        emit({type: "command_output", text: "Local command output"});
        response({agentInvoked: false});
        break;
      }
      response({});
      if (scenario === "provider-error") { result("error"); break; }
      if (scenario === "aborted") { result("aborted"); break; }
      if (scenario === "dialog") {
        emit({type: "extension_ui_request", id: "approval-1", method: "select", title: "Approve tool?", options: ["Approve", "Deny"]});
        break;
      }
      // An unrelated prompt result may not end this turn.
      emit({type: "prompt_result", id: "another-prompt", status: "completed", agentInvoked: true, sessionSettled: true});
      delta("thinking_delta", "Plan");
      delta("text_delta", "Hello");
      emit({type: "message_update", messageId: "msg-2", message: {role: "assistant"}, assistantMessageEvent: {type: "text_end", contentIndex: 0, content: "Hello"}});
      emit({type: "message_end", messageId: "msg-2", message: {role: "assistant", content: [{type: "text", text: "Hello"}], usage: {input: 12, output: 3}}});
      result("completed", false);
      // Background delivery after the first yield must be retained.
      delta("text_delta", " from background");
      emit({type: "session_settled"});
      break;
    case "extension_ui_response":
      assert.equal(command.id, "approval-1");
      assert.equal(command.value, "Approve");
      delta("text_delta", "Approved response");
      result("completed");
      break;
    case "abort": response({}); break;
    default: throw new Error(`Unexpected command ${command.type}`);
  }
});
