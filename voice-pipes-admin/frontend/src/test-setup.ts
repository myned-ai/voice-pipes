import '@testing-library/jest-dom';
import { beforeEach } from 'vitest';
import { installMatchMediaMock, setMockViewport } from './test/viewport';

installMatchMediaMock();

beforeEach(() => {
  // Default every test to a 1440px desktop viewport so existing desktop
  // tests behave identically unless a test explicitly calls setMockViewport().
  setMockViewport(1440, { coarsePointer: false });
});
