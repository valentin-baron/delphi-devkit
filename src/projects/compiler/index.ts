/**
 * Usage: PROBLEMMATCHER_REGEX[+!!Runtime.projects.compiler.configuration?.usePrettyFormat]
 */
export const PROBLEMMATCHER_REGEX: RegExp[] = [
  // Standard format: C:\path\file.pas(412,5): Error E2003: message [project.dproj]
  // Capture groups: [1]=filepath, [2]=line, [3]=column, [4]=severity, [5]=code, [6]=message
  /^(.*?)\((\d+)(?:,(\d+))?\):\s+(.*?)\s+([A-Z]\d+):\s+(.*?)(?:\s+\[.*\])?$/,
  // Pretty format: [WARN][W1029] C:\path\file.pas (line 412): message
  // Capture groups: [1]=WARN, [2]=W1029, [3]=filepath, [4]=line, [5]=message
  /^\[(\w+)\]\[(\w+)\] (.*?) \(line (\d+)\): (.*)$/,
];
