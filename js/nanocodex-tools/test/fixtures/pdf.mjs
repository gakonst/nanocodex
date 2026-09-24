import { deflateSync } from "node:zlib";
import { Buffer } from "node:buffer";
// Synthetic two-page PDF with compressed content, a Unicode map, and columns.
export function pdf({ rotated = false, fragments = false } = {}) {
  const stream = data => {
    const bytes = deflateSync(Buffer.from(data));
    return Buffer.concat([Buffer.from(`<< /Length ${bytes.length} /Filter /FlateDecode >>\nstream\n`), bytes, Buffer.from("\nendstream")]);
  };
  const objects = [
    "<< /Type /Catalog /Pages 2 0 R >>",
    "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>",
    `<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] ${rotated ? "/Rotate 90" : ""} /Resources << /Font << /F1 5 0 R /F2 9 0 R >> >> /Contents 6 0 R >>`,
    "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 5 0 R >> >> /Contents 7 0 R >>",
    "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /ToUnicode 8 0 R >>",
    stream(rotated ? "BT /F1 12 Tf 0 1 -1 0 100 100 Tm (Hello) Tj 60 0 Td (world) Tj ET"
      : fragments ? "BT /F1 12 Tf 50 740 Td (micro) Tj /F2 12 Tf (scope) Tj /F1 12 Tf ( works) Tj ET"
      : "BT /F1 12 Tf 50 740 Td (Alpha) Tj 150 0 Td (Beta) Tj -150 -24 Td (Amount: 100) Tj ET"),
    stream("BT /F1 12 Tf 50 740 Td (Page two: X) Tj ET"),
    stream("/CIDInit /ProcSet findresource begin 12 dict begin begincmap /CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def /CMapName /Test def /CMapType 2 def 1 begincodespacerange <00> <FF> endcodespacerange 1 beginbfrange <00> <FF> <0000> endbfrange 1 beginbfchar <58> <03A9> endbfchar endcmap CMapName currentdict /CMap defineresource pop end end"),
    "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica-Bold >>",
  ];
  let bytes = Buffer.from("%PDF-1.7\n");
  const offsets = [0];
  for (let i = 0; i < objects.length; i++) {
    offsets.push(bytes.length);
    bytes = Buffer.concat([bytes, Buffer.from(`${i + 1} 0 obj\n`), Buffer.from(objects[i]), Buffer.from("\nendobj\n")]);
  }
  const xref = bytes.length;
  return Buffer.concat([bytes, Buffer.from(`xref\n0 ${objects.length + 1}\n0000000000 65535 f \n${offsets.slice(1).map(n => `${String(n).padStart(10, "0")} 00000 n \n`).join("")}trailer\n<< /Size ${objects.length + 1} /Root 1 0 R >>\nstartxref\n${xref}\n%%EOF\n`)]);
}
