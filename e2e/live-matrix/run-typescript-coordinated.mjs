// Final, serialized TypeScript phase for account-wide/shared-state operations.
// DO NOT run alongside another live lane.
import { readFileSync, writeFileSync } from 'node:fs';
if(process.env.CADENYA_LIVE_MATRIX_COORDINATED!=='typescript'){console.error('refusing coordinated sweep: set CADENYA_LIVE_MATRIX_COORDINATED=typescript');process.exit(2)}
const envURL=new URL('../../../../../.env.development',import.meta.url);const envText=readFileSync(envURL,'utf8');
for(const line of envText.split('\n')){const m=line.match(/^\s*(?:export\s+)?([A-Z0-9_]+)=(.*)$/);if(m&&process.env[m[1]]===undefined)process.env[m[1]]=m[2].trim().replace(/^['"]|['"]$/g,'')}
const {default:Cadenya}=await import('../../gen/typescript/dist/index.js');const controller=new Cadenya();let client=controller;const RUN=`coord-ts-${Date.now().toString(36)}`,opts=()=>({signal:AbortSignal.timeout(60000)}),resultURL=new URL('./results-typescript.json',import.meta.url),report=JSON.parse(readFileSync(resultURL,'utf8')),operations=report.operations;
const done=id=>operations[id]={status:'completed',evidence:`real api.cadenya.com: serialized TypeScript coordinated operation succeeded (${RUN}); secret/body not persisted`};const fail=(id,e)=>operations[id]={status:'failed',evidence:`coordinated TypeScript call failed: ${String(e?.message??e).replace(/\s+/g,' ').slice(0,180)}`};const block=(id,w)=>operations[id]={status:'blocked',evidence:`coordinated TypeScript phase: ${w}`};async function op(id,fn){try{const v=await fn();done(id);return v}catch(e){fail(id,e)}}const rid=v=>v?.metadata?.id??v?.id;
try{
 const currentGlobal=await controller.apiKeys.retrieveGlobal(opts());const elevatedToken=currentGlobal?.spec?.token;if(!elevatedToken)throw Error('RetrieveGlobal did not return its token');client=new Cadenya({apiKey:elevatedToken});
 await op('GlobalAPIKeyService_DisableGlobalAPIKey',()=>controller.apiKeys.disableGlobal(opts()));await op('GlobalAPIKeyService_EnableGlobalAPIKey',()=>controller.apiKeys.enableGlobal(opts()));await op('GlobalAPIKeyService_RotateGlobalAPIKey',()=>controller.apiKeys.rotateGlobal(opts()));const fresh=await controller.apiKeys.retrieveGlobal(opts());if(fresh?.spec?.token)client=new Cadenya({apiKey:fresh.spec.token});else throw Error('RetrieveGlobal omitted fresh token after rotation');
 await op('AccountService_RotateChallengeToken',()=>client.accounts.rotateChallengeToken(opts()));
 await op('AccountService_RotateWebhookSigningKey',()=>client.accounts.rotateWebhookSigningKey(opts()));
 for(const id of ['WorkspaceAdminService_ListProfiles','WorkspaceAdminService_ListAccountWorkspaces','WorkspaceAdminService_CreateWorkspace','WorkspaceAdminService_GetWorkspace','WorkspaceAdminService_ArchiveWorkspace','WorkspaceAdminService_UpdateWorkspace','WorkspaceAdminService_ListWorkspaceMembers','WorkspaceAdminService_AddWorkspaceMember','WorkspaceAdminService_RemoveWorkspaceMember'])block(id,'known credential role lacks account-admin authorization');
 let pk=await op('AIProviderKeyService_CreateAIProviderKey',()=>client.aiProviderKeys.create({metadata:{name:`${RUN}-provider`},spec:{provider:'AI_PROVIDER_OPENAI',credentials:{type:'apiKey',apiKey:{apiKey:`invalid-${RUN}`}}}},opts()));if(pk){await op('AIProviderKeyService_UpdateAIProviderKey',()=>client.aiProviderKeys.update(rid(pk),{metadata:{name:`${RUN}-provider-updated`},updateMask:'metadata.name'},opts()));await op('AIProviderKeyService_DeleteAIProviderKey',()=>client.aiProviderKeys.delete(rid(pk),undefined,opts()))}
 const models=await client.models.list({limit:50,includeInfo:true},opts()),safe=models.items.find(m=>(m.info?.agentVariationCount??0)===0);if(safe){await op('ModelService_DisableModel',()=>client.models.disable(rid(safe),undefined,opts()));await op('ModelService_EnableModel',()=>client.models.enable(rid(safe),undefined,opts()));await op('ModelService_SwapModelOnVariations',()=>client.models.swapOnVariations({modelSwaps:[{currentModelId:rid(safe),nextModelId:rid(safe),disableCurrentAfterSwap:false}]},opts()))}else{block('ModelService_DisableModel','no unassigned model');block('ModelService_EnableModel','no unassigned model');block('ModelService_SwapModelOnVariations','no unassigned model')};
 const objectiveCases={
  ObjectiveService_CreateObjectiveFeedback:['FEEDBACK',id=>client.objectives.createFeedback(id,{metadata:{},data:{score:0,comment:`${RUN} live matrix`}},opts())],
  ObjectiveService_CompactObjective:['COMPACT',id=>client.objectives.compact(id,undefined,opts())],
  ObjectiveService_ContinueObjective:['CONTINUE',id=>client.objectives.continue(id,{message:`${RUN} continue`,enqueue:true},opts())],
  ObjectiveService_CancelObjective:['CANCEL',id=>client.objectives.cancel(id,{reason:`${RUN} cancel`},opts())],
 };
 for(const [operationId,[kind,fn]] of Object.entries(objectiveCases)){const oid=process.env[`CADENYA_LIVE_MATRIX_${kind}_OBJECTIVE_ID`];if(oid)await op(operationId,()=>fn(oid));else block(operationId,`CADENYA_LIVE_MATRIX_${kind}_OBJECTIVE_ID missing`)}
 const callCases={
  ObjectiveService_ApproveToolCall:['APPROVE',(oid,tcid)=>client.objectives.approveToolCall(oid,{toolCallId:tcid},opts())],
  ObjectiveService_DenyToolCall:['DENY',(oid,tcid)=>client.objectives.denyToolCall(oid,{toolCallId:tcid,memo:RUN},opts())],
  ObjectiveService_SetToolCallContent:['CONTENT',(oid,tcid)=>client.objectives.setToolCallContent(oid,{toolCallId:tcid,content:[{type:'text',text:{text:RUN}}]},opts())],
 };
 for(const [operationId,[kind,fn]] of Object.entries(callCases)){const oid=process.env[`CADENYA_LIVE_MATRIX_${kind}_OBJECTIVE_ID`],tcid=process.env[`CADENYA_LIVE_MATRIX_${kind}_TOOL_CALL_ID`];if(oid&&tcid)await op(operationId,()=>fn(oid,tcid));else block(operationId,`CADENYA_LIVE_MATRIX_${kind}_OBJECTIVE_ID/TOOL_CALL_ID missing`)}
}finally{report.executedAt=new Date().toISOString();writeFileSync(resultURL,JSON.stringify(report,null,2)+'\n')}
console.log(JSON.stringify(Object.values(operations).reduce((a,x)=>({...a,[x.status]:(a[x.status]??0)+1}),{})));
