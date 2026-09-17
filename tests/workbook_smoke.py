"""Workbook CLI round trips and XML preservation, using only Python's stdlib.

Run after cargo build: python3 tests/workbook_smoke.py [path/to/qd]
Fixtures are generated in a temporary directory; no binary test data is committed.
"""
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import xml.etree.ElementTree as ET
from zipfile import ZipFile, ZIP_DEFLATED, ZIP_STORED

X = 'http://schemas.openxmlformats.org/spreadsheetml/2006/main'
R = 'http://schemas.openxmlformats.org/officeDocument/2006/relationships'
P = 'http://schemas.openxmlformats.org/package/2006/relationships'
T = 'urn:oasis:names:tc:opendocument:xmlns:table:1.0'
O = 'urn:oasis:names:tc:opendocument:xmlns:office:1.0'
TX = 'urn:oasis:names:tc:opendocument:xmlns:text:1.0'


def pack(path, entries):
    with ZipFile(path, 'w', compression=ZIP_DEFLATED) as z:
        for name, value in entries.items():
            z.writestr(name, value, compress_type=ZIP_STORED if name == 'mimetype' else ZIP_DEFLATED)


def xlsx(path):
    entries = {
        '[Content_Types].xml': '''<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
<Default Extension="xml" ContentType="application/xml"/>
<Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>
<Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>
<Override PartName="/xl/worksheets/sheet2.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>
<Override PartName="/xl/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.styles+xml"/>
<Override PartName="/xl/sharedStrings.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sharedStrings+xml"/>
</Types>''',
        '_rels/.rels': f'<Relationships xmlns="{P}"><Relationship Id="book" Type="{R}/officeDocument" Target="xl/workbook.xml"/></Relationships>',
        'xl/workbook.xml': f'<workbook xmlns="{X}" xmlns:r="{R}"><sheets><sheet name="Data" sheetId="1" r:id="r1"/><sheet name="Notes õ" sheetId="2" r:id="r2"/></sheets><calcPr calcId="191029" fullCalcOnLoad="1"/></workbook>',
        'xl/_rels/workbook.xml.rels': f'<Relationships xmlns="{P}"><Relationship Id="r1" Type="{R}/worksheet" Target="worksheets/sheet1.xml"/><Relationship Id="r2" Type="{R}/worksheet" Target="worksheets/sheet2.xml"/><Relationship Id="styles" Type="{R}/styles" Target="styles.xml"/><Relationship Id="strings" Type="{R}/sharedStrings" Target="sharedStrings.xml"/></Relationships>',
        'xl/styles.xml': f'''<styleSheet xmlns="{X}"><fonts count="1"><font><sz val="11"/><name val="Calibri"/></font></fonts><fills count="2"><fill><patternFill patternType="none"/></fill><fill><patternFill patternType="solid"><fgColor rgb="FFFFFF00"/></patternFill></fill></fills><borders count="1"><border/></borders><cellStyleXfs count="1"><xf numFmtId="0" fontId="0" fillId="0" borderId="0"/></cellStyleXfs><cellXfs count="2"><xf numFmtId="0" fontId="0" fillId="0" borderId="0" xfId="0"/><xf numFmtId="0" fontId="0" fillId="1" borderId="0" xfId="0" applyFill="1"/></cellXfs><cellStyles count="1"><cellStyle name="Normal" xfId="0" builtinId="0"/></cellStyles></styleSheet>''',
        'xl/sharedStrings.xml': f'<sst xmlns="{X}" count="2" uniqueCount="2"><si><t>00123</t></si><si><r><t>Tallinn</t></r><r><t xml:space="preserve"> õ</t></r></si></sst>',
        'xl/worksheets/sheet1.xml': f'''<worksheet xmlns="{X}"><dimension ref="A1:D5"/><sheetData>
<row r="1"><c r="A1" t="inlineStr"><is><t>id</t></is></c><c r="B1" t="inlineStr"><is><t>amount</t></is></c><c r="C1" t="inlineStr"><is><t>note</t></is></c><c r="D1" t="inlineStr"><is><t>formula</t></is></c></row>
<row r="2"><c r="A2" t="s"><v>0</v></c><c r="B2" s="1"><v>10</v></c><c r="C2" s="1" t="s"><v>1</v></c><c r="D2"><f>SUM(B2:B3)</f><v>12</v></c></row>
<row r="3"><c r="A3" t="inlineStr"><is><t>00456</t></is></c><c r="B3"><v>2</v></c><c r="D3" t="b"><v>1</v></c></row>
<row r="5"><c r="C5" t="inlineStr"><is><t>end</t></is></c><c r="D5" t="e"><v>#N/A</v></c></row>
</sheetData><autoFilter ref="A1:D5"/></worksheet>''',
        'xl/worksheets/sheet2.xml': f'<worksheet xmlns="{X}"><sheetData><row r="3"><c r="B3" t="inlineStr"><is><t>Untouched õ</t></is></c></row></sheetData></worksheet>',
        'custom/payload.bin': b'opaque bytes\x00\xff\x01',
    }
    pack(path, entries)
    return entries


def ods(path):
    entries = {
        'mimetype': 'application/vnd.oasis.opendocument.spreadsheet',
        'META-INF/manifest.xml': '''<manifest:manifest xmlns:manifest="urn:oasis:names:tc:opendocument:xmlns:manifest:1.0" manifest:version="1.2"><manifest:file-entry manifest:full-path="/" manifest:media-type="application/vnd.oasis.opendocument.spreadsheet"/><manifest:file-entry manifest:full-path="content.xml" manifest:media-type="text/xml"/><manifest:file-entry manifest:full-path="styles.xml" manifest:media-type="text/xml"/></manifest:manifest>''',
        'styles.xml': f'<office:document-styles xmlns:office="{O}" office:version="1.2"><office:styles/></office:document-styles>',
        'content.xml': f'''<office:document-content xmlns:office="{O}" xmlns:table="{T}" xmlns:text="{TX}" xmlns:style="urn:oasis:names:tc:opendocument:xmlns:style:1.0" xmlns:fo="urn:oasis:names:tc:opendocument:xmlns:xsl-fo-compatible:1.0" xmlns:of="urn:oasis:names:tc:opendocument:xmlns:of:1.2" office:version="1.2">
<office:automatic-styles><style:style style:name="highlight" style:family="table-cell"><style:table-cell-properties fo:background-color="#ffff00"/></style:style></office:automatic-styles>
<office:body><office:spreadsheet><table:table table:name="Data">
<table:table-row><table:table-cell office:value-type="string"><text:p>id</text:p></table:table-cell><table:table-cell office:value-type="string"><text:p>amount</text:p></table:table-cell><table:table-cell office:value-type="string"><text:p>note</text:p></table:table-cell><table:table-cell office:value-type="string"><text:p>formula</text:p></table:table-cell></table:table-row>
<table:table-row table:number-rows-repeated="2"><table:table-cell office:value-type="string"><text:p>00123</text:p></table:table-cell><table:table-cell table:style-name="highlight" office:value-type="float" office:value="10"><text:p>10</text:p></table:table-cell><table:table-cell table:number-columns-repeated="2" table:style-name="highlight" office:value-type="string"><text:p>Tallinn õ</text:p></table:table-cell></table:table-row>
<table:table-row/>
<table:table-row><table:table-cell office:value-type="string"><text:p>end</text:p></table:table-cell><table:table-cell office:value-type="float" office:value="2"/><table:table-cell office:value-type="string"><text:p>line 1</text:p><text:p>line 2</text:p></table:table-cell><table:table-cell table:formula="of:=SUM([.B2:.B3])" office:value-type="float" office:value="20"><text:p>20</text:p></table:table-cell></table:table-row>
</table:table><table:table table:name="Notes õ"><table:table-row table:number-rows-repeated="2"/><table:table-row><table:table-cell/><table:table-cell office:value-type="string"><text:p>Untouched õ</text:p></table:table-cell></table:table-row></table:table></office:spreadsheet></office:body></office:document-content>''',
        'custom/payload.bin': b'opaque bytes\x00\xff\x01',
    }
    pack(path, entries)
    return entries


def main():
    binary = str(Path(sys.argv[1] if len(sys.argv) > 1 else 'target/debug/qd').resolve())
    with tempfile.TemporaryDirectory(prefix='quadrille-workbook-') as tmp:
        root = Path(tmp)

        def qd(path, *args, ok=True):
            result = subprocess.run([binary, str(path), *map(str, args)], capture_output=True, timeout=20)
            if ok:
                assert result.returncode == 0, result.stderr.decode()
                return json.loads(result.stdout)
            assert result.returncode != 0, result.stdout.decode()
            return result.stderr.decode()

        for ext, make in [('xlsx', xlsx), ('ods', ods)]:
            source = root / f'source.{ext}'
            entries = make(source)
            original = source.read_bytes()
            assert qd(source, '--sheets')['sheets'] == ['Data', 'Notes õ']
            assert qd(source, '--check')['records'] == 5
            assert qd(source, '--sheet', 'Notes õ', '--read', 'A1:B3')['rows'] == [['', ''], ['', ''], ['', 'Untouched õ']]
            assert qd(source, '--read', 'A2:B2')['rows'] == [['00123', '10']]
            assert 'No sheet' in qd(source, '--sheet', 'missing', '--read', 'A1', ok=False)
            clone = root / f'copy.{ext}'
            qd(source, '--output', clone)
            assert clone.read_bytes() == original

            formula_at = 'D2' if ext == 'xlsx' else 'D5'
            result = qd(source, '--read', formula_at)
            assert formula_at in result['formulas'] and 'cached' in result['formula_results']
            qd(source, '--set', formula_at, 'broken', '--dry-run', ok=False)

            changed = '  New, "quoted" & <õ>\ncity\t 🦀\r_x0041_'
            dest = root / f'edited.{ext}'
            edits = ['--set', 'C2', changed, '--set', 'B2', '00100', '--set', 'B4', '=1+1', '--set', 'C3', '']
            qd(source, *edits, '--output', dest)
            assert source.read_bytes() == original
            assert qd(dest, '--read', 'B2:C2')['rows'] == [['00100', changed]]
            assert qd(dest, '--read', 'B4')['rows'] == [['=1+1']]
            assert qd(dest, '--read', 'C3')['rows'] == [['']]
            assert qd(dest, '--read', formula_at)['formulas'] == result['formulas']
            qd(source, *edits, '--output', dest, ok=False)
            qd(source, *edits, '--output', source, ok=False)
            qd(source, '--output', root / 'bad.ods' if ext == 'xlsx' else root / 'bad.xlsx', ok=False)
            with ZipFile(dest) as z:
                changed_part = 'xl/worksheets/sheet1.xml' if ext == 'xlsx' else 'content.xml'
                for name, value in entries.items():
                    if name != changed_part:
                        assert z.read(name) == (value.encode() if isinstance(value, str) else value), name
                edited_xml = ET.fromstring(z.read(changed_part))
                if ext == 'xlsx':
                    b2 = edited_xml.find(f'.//{{{X}}}c[@r="B2"]')
                    assert b2.attrib['s'] == '1' and b2.attrib['t'] == 'inlineStr'
                    assert edited_xml.find(f'.//{{{X}}}autoFilter').attrib['ref'] == 'A1:D5'
                else:
                    assert z.infolist()[0].filename == 'mimetype' and z.infolist()[0].compress_type == ZIP_STORED
                    table = edited_xml.find(f'.//{{{T}}}table[@{{{T}}}name="Data"]')
                    rows = table.findall(f'{{{T}}}table-row')
                    assert rows[1][1].attrib[f'{{{T}}}style-name'] == 'highlight'
                    old = ET.fromstring(entries['content.xml'])
                    for tag in [f'{{{T}}}table[@{{{T}}}name="Notes õ"]', f'{{{O}}}automatic-styles']:
                        assert ET.tostring(edited_xml.find('.//' + tag)) == ET.tostring(old.find('.//' + tag))
                    assert qd(dest, '--read', 'D2:D3')['rows'] == [['Tallinn õ'], ['Tallinn õ']]

            # Export follows the sorted view; native saves refuse to break references.
            sorted_rows = qd(source, '--sort', 'B:n', '--read', 'A1:D5')['rows']
            csv_dest = root / f'{ext}-sorted.csv'
            qd(source, '--sort', 'B:n', '--output', csv_dest)
            assert qd(csv_dest, '--read', 'A1:D5')['rows'] == sorted_rows
            rejected = root / f'rejected.{ext}'
            assert 'clear the sort' in qd(source, '--sort', 'B:n', '--output', rejected, ok=False)
            assert not rejected.exists()
            qd(source, '--set', 'B2', 'valid', '--set', 'XFD1048576', 'invalid', '--output', rejected, ok=False)
            assert not rejected.exists()
            qd(source, '--set', 'C2', '\x01', '--output', rejected, ok=False)
            assert not rejected.exists()
            # Editing a second time must keep namespace declarations valid.
            twice = root / f'twice.{ext}'
            qd(dest, '--set', 'C2', 'second edit', '--output', twice)
            assert qd(twice, '--read', 'C2')['rows'] == [['second edit']]
            assert source.read_bytes() == original
        # Formula/merge protection and oversized sparse dimensions must fail before publishing.
        protected = root / 'merged.xlsx'
        entries = xlsx(protected)
        part = 'xl/worksheets/sheet1.xml'
        entries[part] = entries[part].replace('</worksheet>', '<mergeCells count="1"><mergeCell ref="B2:C2"/></mergeCells></worksheet>')
        pack(protected, entries)
        qd(protected, '--set', 'B2', 'bad', '--dry-run', ok=False)
        entries[part] = entries[part].replace('r="C5"', 'r="XFD1048576"')
        pack(protected, entries)
        assert 'import limit' in qd(protected, '--check', ok=False)
        special = root / 'special.ods'
        entries = ods(special)
        entries['content.xml'] = entries['content.xml'].replace('Untouched õ', 'Tab<text:tab/>line<text:line-break/>õ')
        pack(special, entries)
        assert qd(special, '--sheet', 'Notes õ', '--read', 'B3')['rows'] == [['Tab\tline\nõ']]
        entries['content.xml'] = entries['content.xml'].replace('table:number-columns-repeated="2"', 'table:number-columns-repeated="2" table:number-columns-spanned="2"')
        pack(special, entries)
        qd(special, '--set', 'C2', 'bad', '--dry-run', ok=False)
        print('Workbook smoke PASS: XLSX/ODS sheets, coordinates, formulas, edits, blanks, repeats, styles, untouched parts, CSV sorting/export, no-clobber, failed batches')


if __name__ == '__main__':
    main()
