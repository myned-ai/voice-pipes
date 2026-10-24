import type { PlaygroundExtension } from "../types";
import { createBlendshapeMeter } from "./blendshape-meter";

/** Extensions bundled with the admin frontend, addressable as `builtin:<name>` in the manifest. */
export const BUILTIN_EXTENSIONS: Record<string, () => PlaygroundExtension> = {
  "blendshape-meter": createBlendshapeMeter,
};
