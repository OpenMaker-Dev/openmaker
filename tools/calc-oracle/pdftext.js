// Text of each page of a PDF, as JSON (PDFKit). Sends no Apple events.
ObjC.import('PDFKit');
function run(args) {
    const doc=$.PDFDocument.alloc.initWithURL($.NSURL.fileURLWithPath(args[0]));
    if (!doc || doc.isNil()) throw Error('cannot read PDF '+args[0]);
    const pages=[];
    for (let i=0;i<doc.pageCount;i++) pages.push(doc.pageAtIndex(i).string.js);
    return JSON.stringify(pages);
}
