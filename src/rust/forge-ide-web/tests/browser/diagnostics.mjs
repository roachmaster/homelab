// Fixture-only screenshots. No page text/URL/error is returned as a diagnostic.
import { join } from 'node:path';

export const SCREENSHOT_MASK = '#connection, #pair-panel, input, textarea, [contenteditable]';
const CREDENTIAL_PATTERN = '\\b[0-9a-f]{64}\\b|-----BEGIN [A-Z ]*PRIVATE KEY-----|\\bBearer\\s+\\S+|\\bAuthorization\\s*:|\\b(?:password|secret|cookie)\\s*[:=]';

export async function screenshotEligibility(page, fixtures, { home = false, docs = false } = {}) {
  const fixture = fixtures.find(item => item.page === page);
  if (!fixture || !page || page.isClosed()) return 'unavailable';
  const url = new URL(page.url());
  const docsPaths = new Set(['/design', '/design/previous-chats', '/design/wireframes']);
  const pathAllowed = docs ? docsPaths.has(url.pathname) : url.pathname === (home ? '/home' : '/');
  const hashAllowed = !url.hash || (docs && url.pathname === '/design/wireframes' && ['#phone', '#desktop'].includes(url.hash));
  if ((home && docs) || url.origin !== fixture.origin || !pathAllowed || url.search || !hashAllowed) return 'skipped_unsafe_page';
  const markers = docs ? ['main h1', 'nav[aria-label="Forge views"]', 'nav[aria-label="Design topics"]']
    : home ? ['main h1', 'main .home-actions'] : ['#pair-panel', '#workbench'];
  if (!(await Promise.all(markers.map(marker => page.locator(marker).count()))).every(count => count === 1)) {
    return 'skipped_unsafe_page';
  }
  // Only a boolean leaves the owned page; never retain/serialize its rendered text.
  const unsafe = await page.evaluate(pattern => new RegExp(pattern, 'i').test(document.body.innerText), CREDENTIAL_PATTERN);
  return unsafe ? 'skipped_credential_pattern' : 'safe';
}

export async function captureFailureScreenshot(report, output, fixtures, enabled) {
  if (!enabled) return;
  report.screenshot('unavailable');
  const page = fixtures.at(-1)?.page;
  const eligibility = await screenshotEligibility(page, fixtures);
  if (eligibility !== 'safe') { report.screenshot(eligibility); return; }
  await page.screenshot({ path: join(output, 'failure.png'), fullPage: false,
    timeout: 3_000, mask: [page.locator(SCREENSHOT_MASK)] });
  report.screenshot('captured');
}
