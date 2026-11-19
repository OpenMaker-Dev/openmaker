// Explicit Unicode Apple event; JXA's ordinary cell.set sends garbled text to FM26.
ObjC.import('Cocoa');
function cc(s) { return s.split('').reduce((a,c)=>(a*256+c.charCodeAt(0))>>>0,0); }
const D=$.NSAppleEventDescriptor;
function spec(cls,key,from) {
    const d=D.recordDescriptor;
    d.setDescriptorForKeyword(D.descriptorWithTypeCode(cc(cls)),cc('want'));
    d.setDescriptorForKeyword(D.descriptorWithEnumCode(cc(typeof key==='number'?'indx':'name')),cc('form'));
    d.setDescriptorForKeyword(typeof key==='number'?D.descriptorWithInt32(key):D.descriptorWithString(key),cc('seld'));
    d.setDescriptorForKeyword(from||D.nullDescriptor,cc('from'));
    return d.coerceToDescriptorType(cc('obj '));
}
// Event class and id per action; every event targets the given PID only.
const EVENTS={open:['aevt','odoc'],quit:['aevt','quit'],eval:['core','setd'],dosc:['misc','dosc'],read:['core','getd']};
function run(args) {
    const pid=Number(args[0]), action=args[1], target=D.descriptorWithProcessIdentifier(pid);
    if (action==='version') return Application(pid).version();
    const [cls,id]=EVENTS[action];
    const e=D.appleEventWithEventClassEventIDTargetDescriptorReturnIDTransactionID(cc(cls),cc(id),target,-1,0);
    let wait=20;
    if (action==='open') {
        const files=D.listDescriptor;
        files.insertDescriptorAtIndex(D.descriptorWithFileURL($.NSURL.fileURLWithPath(args[2])),1);
        e.setParamDescriptorForKeyword(files,cc('----'));
    } else if (action==='eval') {
        const cell=spec('ccel','Input',spec('reco',1,spec('cTBL',1,spec('cDB ','calc-oracle.fmp12'))));
        e.setParamDescriptorForKeyword(cell,cc('----'));
        e.setParamDescriptorForKeyword(D.descriptorWithString(args[2]),cc('data'));
    } else if (action==='dosc') {
        // do script: a FileMaker script of a database, by name (fmpro.sdef)
        e.setParamDescriptorForKeyword(spec('cSCP',args[3],spec('cDB ',args[2])),cc('----'));
        wait=90;
    } else if (action==='read') {
        // the first record's cell of a table occurrence (a global probe field)
        e.setParamDescriptorForKeyword(spec('ccel',args[4],spec('reco',1,spec('cTBL',args[3],spec('cDB ',args[2])))),cc('----'));
        e.setParamDescriptorForKeyword(D.descriptorWithTypeCode(cc('utxt')),cc('rtyp'));
        wait=10;
    }
    const err=Ref(), reply=e.sendEventWithOptionsTimeoutError(3,wait,err);
    if (!reply || typeof reply.paramDescriptorForKeyword !== 'function') {
        // do script may outlive the reply (a paused script); the runner then waits for its answers
        if (action==='dosc') return 'no reply';
        throw Error('Apple event did not return a reply');
    }
    const failure=reply.paramDescriptorForKeyword(cc('errn'));
    if (failure && failure.int32Value) throw Error('FileMaker Apple event error '+failure.int32Value);
    if (action==='read') {
        const r=reply.paramDescriptorForKeyword(cc('----'));
        return r ? r.stringValue.js : '';
    }
    if (action==='dosc') {
        const r=reply.paramDescriptorForKeyword(cc('----'));
        return r && r.stringValue ? 'reply '+r.stringValue.js : 'reply';
    }
    if (action!=='eval') return action;
    return Application(pid).databases.byName('calc-oracle.fmp12').tables[0].records[0].cells.byName('Output').get();
}
