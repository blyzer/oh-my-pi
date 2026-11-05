test('passes', () => { expect(1 + 1).toBe(2); });
test('fails', () => { expect(1 + 1).toBe(3); });
test.skip('skipped', () => {});
describe('setup', () => {
  beforeAll(() => { throw new Error('intentional error'); });
  test('errors', () => {});
});
