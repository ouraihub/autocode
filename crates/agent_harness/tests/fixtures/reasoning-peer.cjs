const readline = require("node:readline");
const mode = process.argv.find(value => ["pi", "omp", "dsh"].includes(value));
let thinking = "medium";
let applied = false;
function emit(value) { console.log(JSON.stringify(value)); }
function options() {
  return [{id: "provider_effort", category: "thought_level", type: "select", currentValue: thinking,
    options: [{value: "off", name: "Off"}, {value: "high", name: "High"}]}];
}
if (mode === "omp") emit({type: "ready"});
readline.createInterface({input: process.stdin}).on("line", line => {
  const command = JSON.parse(line);
  const kind = command.type || command.method;
  if (mode === "dsh") {
    const result = value => emit({jsonrpc: "2.0", id: command.id, result: value});
    if (kind === "initialize") result({protocolVersion: 1});
    else if (kind === "session/new") result({sessionId: "test-session", configOptions: options()});
    else if (kind === "session/set_config_option") {
      if (command.params.configId !== "provider_effort") throw new Error("wrong config id");
      thinking = command.params.value;
      applied = true;
      result({configOptions: options()});
    } else if (kind === "session/prompt") {
      if (!applied) throw new Error("prompt preceded reasoning selection");
      result({stopReason: "end_turn"});
    }
    return;
  }
  const response = data => emit({type: "response", id: command.id, command: kind, success: true, data});
  if (kind === "get_state") response({model: mode === "omp" ? {provider: "test", id: "test-model"} : "test-model", sessionFile: "test-session", thinkingLevel: thinking});
  else if (kind === "set_thinking_level") { thinking = command.level; applied = true; response({}); }
  else if (kind === "prompt") {
    if (!applied) throw new Error("prompt preceded reasoning selection");
    emit({type: "message_update", message: {role: "assistant"}, assistantMessageEvent: {type: "text_delta", contentIndex: 0, delta: thinking}});
    if (mode === "omp") {
      response({});
      emit({type: "prompt_result", id: command.id, status: "completed", agentInvoked: true, sessionSettled: true});
    } else emit({type: "agent_settled", sessionFile: "test-session"});
  } else response({});
});
