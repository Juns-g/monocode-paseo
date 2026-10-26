// Exact daemon catalog values, shared by native create/configure and their tests.
export function fullAccessMode(modes) {
  const match=modes.find(m=>['dont_ask','bypassPermissions','full-access','full'].includes(m.id));
  if(!match)throw Error('该 Paseo provider 未公布全权限模式，无法创建；请在 Paseo 检查。');
  return match.id;
}
export function featureKey(id){return `paseo.${id}`;}
export function nativeSettings(agent) {
  const result={};
  if(agent.thinkingOptionId!=null)result[agent.provider==='codex'?'reasoningEffort':'effort']=agent.thinkingOptionId;
  for(const f of agent.features??[])if(f.value!=null)result[featureKey(f.id)]=String(f.value);
  return result;
}
export function nativeModel(model,features) {
  const settings=[];
  if(model.thinkingOptions?.length)settings.push({id:model.provider==='codex'?'reasoningEffort':'effort',label:'Thinking',kind:'select',
    value:model.defaultThinkingOptionId??model.thinkingOptions.find(o=>o.isDefault)?.id??'',options:model.thinkingOptions.map(o=>({value:o.id,label:o.label}))});
  for(const f of features)settings.push({id:featureKey(f.id),label:f.label,kind:'select',value:String(f.value??''),
    options:f.type==='toggle'?[{value:'false',label:'Off'},{value:'true',label:'On'}]:(f.options??[]).map(o=>({value:o.id,label:o.label}))});
  return {id:model.id,name:model.label,isDefault:!!model.isDefault,hidden:model.isSelectable===false,settings};
}
export function configuration(model,features,values={}) {
  const result={model:model.id},featureValues={};
  for(const [key,value] of Object.entries(values)) {
    if(['effort','reasoningEffort'].includes(key)) {
      if(!model.thinkingOptions?.some(o=>o.id===value))throw Error('模型未公布此思考选项；未应用配置');
      result.thinkingOptionId=value;continue;
    }
    if(!key.startsWith('paseo.') && Object.hasOwn(values,featureKey(key)))continue;
    let id=key.startsWith('paseo.')?key.slice(6):key==='serviceTier'?'service_tier':key;
    const feature=features.find(f=>f.id===id);
    // Native defaults are not provider feature requests.
    if(!feature && ['fast','thinking'].includes(key) && value==='false')continue;
    if(!feature)throw Error(`Paseo 未公布 feature: ${id}`);
    if(feature.type==='toggle') {
      if(!['true','false'].includes(value))throw Error('Invalid feature toggle');
      featureValues[id]=value==='true';
    }else {
      if(!feature.options?.some(o=>o.id===value))throw Error('Invalid feature option');
      featureValues[id]=value;
    }
  }
  // Resetting a model without an explicit effort uses the daemon/provider default.
  result.thinkingOptionId??=null;
  if(Object.keys(featureValues).length)result.featureValues=featureValues;
  return result;
}
