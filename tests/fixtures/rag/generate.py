#!/usr/bin/env python3
"""Generate anonymous, synthetic extraction acceptance fixtures (standard library)."""
import pathlib, sys, zipfile
root=pathlib.Path(sys.argv[1]); root.mkdir(parents=True,exist_ok=True)
ct='http://schemas.openxmlformats.org/package/2006/content-types'
r='http://schemas.openxmlformats.org/officeDocument/2006/relationships'
pkg='http://schemas.openxmlformats.org/package/2006/relationships'
def archive(name, files):
 with zipfile.ZipFile(root/name,'w',zipfile.ZIP_DEFLATED) as z:
  for key,value in files.items(): z.writestr(key,value)
def types(entries):
 return f'<Types xmlns="{ct}"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/>'+''.join(f'<Override PartName="/{part}" ContentType="{mime}"/>' for part,mime in entries)+'</Types>'
def rels(items):return f'<Relationships xmlns="{pkg}">'+''.join(f'<Relationship Id="{i}" Type="{r}/{t}" Target="{target}"/>' for i,t,target in items)+'</Relationships>'
w='http://schemas.openxmlformats.org/wordprocessingml/2006/main'
archive('synthetic.docx',{
 '[Content_Types].xml':types([('word/document.xml','application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml')]),
 '_rels/.rels':rels([('rId1','officeDocument','word/document.xml')]),
 'word/document.xml':f'<w:document xmlns:w="{w}"><w:body><w:p><w:r><w:t>SHELLBOY_DOCX_V1 日本語の抽出確認</w:t></w:r></w:p><w:tbl><w:tr><w:tc><w:p><w:r><w:t>表の項目</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>42</w:t></w:r></w:p></w:tc></w:tr></w:tbl><w:sectPr/></w:body></w:document>'})
a='http://schemas.openxmlformats.org/drawingml/2006/main';p='http://schemas.openxmlformats.org/presentationml/2006/main'
files={'[Content_Types].xml':types([('ppt/presentation.xml','application/vnd.openxmlformats-officedocument.presentationml.presentation.main+xml')]+[(f'ppt/slides/slide{i}.xml','application/vnd.openxmlformats-officedocument.presentationml.slide+xml') for i in [1,2]]),
 '_rels/.rels':rels([('rId1','officeDocument','ppt/presentation.xml')]),
 'ppt/presentation.xml':f'<p:presentation xmlns:p="{p}" xmlns:r="{r}"><p:sldIdLst><p:sldId id="256" r:id="rId1"/><p:sldId id="257" r:id="rId2"/></p:sldIdLst><p:sldSz cx="9144000" cy="6858000"/></p:presentation>',
 'ppt/_rels/presentation.xml.rels':rels([(f'rId{i}','slide',f'slides/slide{i}.xml') for i in [1,2]])}
for i in [1,2]:files[f'ppt/slides/slide{i}.xml']=f'<p:sld xmlns:p="{p}" xmlns:a="{a}"><p:cSld name="Synthetic {i}"><p:spTree><p:sp><p:nvSpPr><p:cNvPr id="2" name="Text"/><p:cNvSpPr/><p:nvPr/></p:nvSpPr><p:spPr/><p:txBody><a:bodyPr/><a:lstStyle/><a:p><a:r><a:t>SHELLBOY_PPTX_{i} 日本語スライド{i}</a:t></a:r></a:p></p:txBody></p:sp></p:spTree></p:cSld></p:sld>'
archive('synthetic.pptx',files)
s='http://schemas.openxmlformats.org/spreadsheetml/2006/main'
files={'[Content_Types].xml':types([('xl/workbook.xml','application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml')]+[(f'xl/worksheets/sheet{i}.xml','application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml') for i in [1,2]]),
 '_rels/.rels':rels([('rId1','officeDocument','xl/workbook.xml')]),
 'xl/workbook.xml':f'<workbook xmlns="{s}" xmlns:r="{r}"><sheets><sheet name="資料一" sheetId="1" r:id="rId1"/><sheet name="資料二" sheetId="2" r:id="rId2"/></sheets></workbook>',
 'xl/_rels/workbook.xml.rels':rels([(f'rId{i}','worksheet',f'worksheets/sheet{i}.xml') for i in [1,2]])}
for i in [1,2]:files[f'xl/worksheets/sheet{i}.xml']=f'<worksheet xmlns="{s}"><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>SHELLBOY_XLSX_{i} 日本語表{i}</t></is></c><c r="B1"><v>{i*42}</v></c></row></sheetData></worksheet>'
archive('synthetic.xlsx',files)
objects=[b'<< /Type /Catalog /Pages 2 0 R >>',b'<< /Type /Pages /Kids [3 0 R] /Count 1 >>',b'<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>',b'<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>']
content=b'BT /F1 12 Tf 72 720 Td (SHELLBOY_PDF_V1 synthetic extraction document) Tj ET'
objects.append(b'<< /Length '+str(len(content)).encode()+b' >>\nstream\n'+content+b'\nendstream')
pdf=bytearray(b'%PDF-1.4\n'); offsets=[0]
for i,obj in enumerate(objects,1):offsets.append(len(pdf));pdf.extend(f'{i} 0 obj\n'.encode()+obj+b'\nendobj\n')
xref=len(pdf);pdf.extend(f'xref\n0 {len(objects)+1}\n0000000000 65535 f \n'.encode())
for offset in offsets[1:]:pdf.extend(f'{offset:010} 00000 n \n'.encode())
pdf.extend(f'trailer\n<< /Size {len(objects)+1} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n'.encode());(root/'synthetic.pdf').write_bytes(pdf)
(root/'synthetic.md').write_text('# Anonymous RAG fixture\nSHELLBOY_MARKDOWN_V1 日本語の検索と保存版取得。\n')
print(root)
