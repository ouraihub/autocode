const readline = require('node:readline');
const scenario = process.argv[2];
const emit = frame => console.log(JSON.stringify(frame));
readline.createInterface({input: process.stdin}).on('line', line => {
  const command = JSON.parse(line);
  const response = data => emit({type:'response',id:command.id,command:command.type,success:true,data});
  if (command.type === 'get_state') {
    response({model:{provider:'fixture',id:'fixture-chat'},thinkingLevel:'off',sessionFile:'fixture-session'});
  } else if (command.type === 'prompt') {
    response({});
    if (scenario === 'error') {
      emit({type:'message_end',message:{role:'assistant',stopReason:'error',errorMessage:'Controlled provider failure'}});
    } else if (scenario !== 'aborted') {
      emit({type:'message_update',assistantMessageEvent:{type:'thinking_delta',delta:'Plan'}});
      emit({type:'message_update',assistantMessageEvent:{type:'thinking_end',content:'Plan'}});
      emit({type:'tool_execution_start',toolCallId:'bash-1',toolName:'bash',args:{command:'echo fixture'}});
      emit({type:'tool_execution_end',toolCallId:'bash-1',isError:false,result:{content:[{type:'text',text:'fixture output'}]}});
      emit({type:'message_update',assistantMessageEvent:{type:'text_delta',delta:'Hello '}});
      emit({type:'message_update',assistantMessageEvent:{type:'text_delta',delta:'Pi'}});
      emit({type:'message_update',assistantMessageEvent:{type:'text_end',content:'Hello Pi'}});
      emit({type:'message_end',message:{role:'assistant',stopReason:'stop'}});
    }
    emit({type:'agent_settled',aborted:scenario === 'aborted'});
  }
});
