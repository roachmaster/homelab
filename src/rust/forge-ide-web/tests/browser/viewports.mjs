// Fixed fixture-only CSS viewport coverage, not iOS, WebKit or touch emulation.
// Dimensions verified against the pinned Playwright devices registry at startup.
// Source: https://github.com/microsoft/playwright/blob/v1.64.1/packages/isomorphic/deviceDescriptorsSource.json
const profiles = [
  ['iphone-se3-portrait', 'iPhone SE (3rd gen)', 375, 667],
  ['iphone-se3-landscape', 'iPhone SE (3rd gen) landscape', 667, 375],
  ['iphone13-portrait', 'iPhone 13', 390, 664],
  ['iphone13-landscape', 'iPhone 13 landscape', 750, 342],
  ['iphone14-pro-max-portrait', 'iPhone 14 Pro Max', 430, 740],
  ['iphone14-pro-max-landscape', 'iPhone 14 Pro Max landscape', 814, 380],
  ['ipad-mini-portrait', 'iPad Mini', 768, 1024],
  ['ipad-mini-landscape', 'iPad Mini landscape', 1024, 768],
  ['ipad-pro11-portrait', 'iPad Pro 11', 834, 1194],
  ['ipad-pro11-landscape', 'iPad Pro 11 landscape', 1194, 834],
];
export const VIEWPORTS = Object.freeze(profiles.map(([key, profile, width, height]) => Object.freeze({
  id: `real.viewport.${key}`, profile, width, height,
  screenshot: `ide-viewport-${key}.png`,
})));
export function verifyViewportProfiles(devices) {
  for (const item of VIEWPORTS) {
    const actual = devices[item.profile]?.viewport;
    if (actual?.width !== item.width || actual?.height !== item.height) throw new Error('Pinned viewport registry mismatch');
  }
}
