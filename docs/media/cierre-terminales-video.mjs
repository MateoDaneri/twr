// Synthetic recreation of the manually observed Development flow.
// No screen recording, personal session, or unobserved additional recovery.
export function renderFrame(t) {
  const second = t >= 7 && t < 13;
  const last = t >= 13;
  const local = second ? t - 7 : t;
  const closed = !last && local >= 4.7;
  const chord = ['Cmd+W'];
  const keys = last ? 'Cmd+W' : local < 3 ? '' : chord[0];
  const tabs = last ? '' : closed
    ? '<rect x="45" y="147" width="190" height="48" rx="8" fill="#244a59"/><text x="65" y="178">1  terminal</text>'
    : '<text x="65" y="178">1  vecina</text><rect x="292" y="147" width="250" height="48" rx="8" fill="#244a59"/><text x="314" y="178">2  elegida</text>';
  const title = last ? 'Cmd+W: cerrar la última ventana' : second ? 'Cmd+W desde Terminal Normal' : 'Cmd+W desde Terminal Input';
  const lines = last
    ? ['[exited]', 'La app sigue abierta. No se crea otro shell.']
    : closed ? ['VECINA VIVA', '', 'Su proceso y su contenido se conservan.']
    : ['TERMINAL DESCARTABLE', '', 'sh-3.2$ ▍', '', 'La otra ventana permanece abierta.'];
  const status = last ? 'Última terminal finalizada' : closed ? 'Terminal finalizada · otras terminales conservadas' : second ? 'NORMAL' : 'INPUT';
  return `<svg xmlns="http://www.w3.org/2000/svg" width="1280" height="720" viewBox="0 0 1280 720">
  <rect width="1280" height="720" fill="#eef2f5"/>
  <g font-family="Arial,sans-serif" fill="#172b38">
  <text x="46" y="45" font-size="21" font-weight="bold">RECREACIÓN SINTÉTICA · TWR</text>
  <text x="46" y="92" font-size="33" font-weight="bold">${title}</text>
  <rect x="32" y="125" width="1216" height="475" rx="15" fill="#0b2732"/>
  <g font-family="Menlo,monospace" font-size="24" fill="#dcebf0">${tabs}
  <path d="M45 205 H1235" stroke="#48636c"/>
  ${lines.map((line,i) => `<text x="62" y="${252 + i * 47}">${line}</text>`).join('')}
  <rect x="45" y="543" width="1190" height="42" fill="#244451"/><text x="62" y="571" font-size="20">${status}</text></g>
  <text x="48" y="650" font-family="Menlo,monospace" font-size="30" font-weight="bold">${keys || 'Elegir una terminal descartable'}</text>
  <text x="48" y="695" font-size="19">Animación explicativa basada en la revisión manual. No es una grabación ni prueba de ejecución.</text>
  </g></svg>`;
}
