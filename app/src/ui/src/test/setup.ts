import "@testing-library/jest-dom/vitest";
import { configure } from "@solidjs/testing-library";

// findBy... and waitFor give up after 1 s by default, which a busy CI
// runner exceeds; a passing test never waits this long
configure({ asyncUtilTimeout: 10_000 });
