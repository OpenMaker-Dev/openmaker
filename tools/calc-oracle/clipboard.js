ObjC.import('Cocoa');
function run(args) {
 const p=$.NSPasteboard.generalPasteboard;
 if(args[0]==='set') {
  const saved=$.NSMutableArray.alloc.init;
  const items=p.pasteboardItems;
  for(let i=0;i<items.count;i++) {
   const item=items.objectAtIndex(i), dict=$.NSMutableDictionary.alloc.init;
   const types=item.types;
   for(let j=0;j<types.count;j++) { const t=types.objectAtIndex(j);dict.setObjectForKey(item.dataForType(t),t); }
   saved.addObject(dict);
  }
  if(!saved.writeToFileAtomically(args[1],true)) throw Error('clipboard backup failed');
  const xml=$.NSString.stringWithContentsOfFileEncodingError(args[2],$.NSUTF8StringEncoding,null);
  if(!xml || !xml.length) throw Error('missing snippet');
  p.clearContents;
  p.setStringForType(xml,'CorePasteboardFlavorType 0x584D5353');
 } else {
  const saved=$.NSArray.arrayWithContentsOfFile(args[1]), items=$.NSMutableArray.alloc.init;
  for(let i=0;i<saved.count;i++) {
   const dict=saved.objectAtIndex(i), item=$.NSPasteboardItem.alloc.init, keys=dict.allKeys;
   for(let j=0;j<keys.count;j++) {const t=keys.objectAtIndex(j);item.setDataForType(dict.objectForKey(t),t);}
   items.addObject(item);
  }
  p.clearContents; if(items.count) p.writeObjects(items);
 }
 return 'ok';
}
