import assert from "node:assert/strict";
import { writeFile } from "node:fs/promises";
import { createMcpStreamableHttpClient } from "../adapters/openai/mcp_streamable_http_client.js";
import { assertObservationReceipt, assertStepReceipt, assertTypedPng } from "./codex_journey_assertions.mjs";
const serial="fixture-device";
const clients=[0,1].map(i=>createMcpStreamableHttpClient({endpoint:process.env.MCP_ENDPOINT,clientName:`direct-http-fixture-${i}`,fetchImpl:(url,options)=>fetch(url,{...options,signal:AbortSignal.timeout(60000)})}));
function output(result, ok = true) {
  assert.equal(Boolean(result.isError), !ok);
  assert.ok(result.structuredContent === undefined);
  assert.ok(Array.isArray(result.content));
  return result.content.map(part=> part.type==="text"?{type:"input_text",text:part.text}:part.type==="image"?{type:"input_image",detail:"high",image_url:`data:${part.mimeType};base64,${part.data}`}:{type:part.type});
}
const checks=[];
try {
  for(const client of clients) { const tools=await client.listTools(); assert.equal(tools.length,45); }
  assert.notEqual(clients[0].getSessionId(),clients[1].getSessionId());
  if(process.env.MCP_HTTP_CONTRACT_PHASE === "recover") {
    const status=await clients[1].callTool("android.get_action_status",{action_id:"http-success"});
    assert.equal(status.state,"complete");
    assert.equal(status.receipt.serial,serial);
    assert.equal(status.receipt.executed_action_count,1);
    assert.equal(status.do_not_replay,true);
    assert.equal(status.retry_safe,false);
    assert.equal(typeof status.original_provider_epoch,"string");
    checks.push({gate:"offline_restart_recovery"});
  } else {
  const frame=output(await clients[0].callTool("android.observe",{serial}));
  assertObservationReceipt(frame,serial); checks.push({gate:"observe",image:assertTypedPng(frame)});
  const success={serial,action_id:"http-success",actions:[{tool:"android.input.keyevent",arguments:{keycode:"KEYCODE_HOME"}}]};
  const first=output(await clients[0].callTool("android.step",success));
  assertStepReceipt(first,{serial,actionId:success.action_id,success:true,count:1}); checks.push({gate:"successful_step",image:assertTypedPng(first)});
  const duplicate=output(await clients[1].callTool("android.step",success));
  assertStepReceipt(duplicate,{serial,actionId:success.action_id,success:true,count:1,duplicate:true}); checks.push({gate:"cross_session_duplicate",image:assertTypedPng(duplicate)});
  const failure={serial,action_id:"http-partial-failure",actions:[{tool:"android.input.keyevent",arguments:{keycode:"KEYCODE_BACK"}},{tool:"android.tap_element",arguments:{selector:{resource_id:"fixture:id/never"},timeout_secs:1}},{tool:"android.input.keyevent",arguments:{keycode:"KEYCODE_HOME"}}]};
  const failed=output(await clients[0].callTool("android.step",failure),false);
  assertStepReceipt(failed,{serial,actionId:failure.action_id,success:false,count:1,attempted:2,failedIndex:1}); checks.push({gate:"partial_failure",image:assertTypedPng(failed)});
  const concurrent={serial,action_id:"http-concurrent-once",actions:[{tool:"android.input.keyevent",arguments:{keycode:"KEYCODE_HOME"}}]};
  const pair=await Promise.all(clients.map(client=>client.callTool("android.step",concurrent)));
  const receipts=pair.map(result=>JSON.parse(result.content.find(part=>part.type==="text").text));
  assert.deepEqual(receipts.map(receipt=>receipt.duplicate_action_id).sort(),[false,true]);
  for(const result of pair) assertTypedPng(output(result));
  checks.push({gate:"concurrent_sessions_one_mutation"});
  const status=await clients[1].callTool("android.get_action_status",{action_id:success.action_id});
  assert.equal(status.state,"complete"); assert.equal(status.do_not_replay,true); checks.push({gate:"durable_status"});
  }
  await writeFile(process.env.DIRECT_HTTP_EVIDENCE,JSON.stringify({status:"passed",device_kind:"deterministic-simulated-sdk",actual_android:false,consumer:"direct HTTP MCP client",codex_consumer_tested:false,checks},null,2)+"\n");
  console.log(`Direct HTTP simulated-device smoke passed: ${checks.length} checks across two MCP sessions.`);
} finally {await Promise.allSettled(clients.map(client=>client.close()));}
