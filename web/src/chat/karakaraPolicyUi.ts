// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

import type { KarakaraPosture } from "../types/config";

export function postureLabel(posture: KarakaraPosture): string {
  switch (posture) {
    case "off":
      return "Off";
    case "audit":
      return "Audit";
    case "enforce":
      return "Enforce";
    default:
      return posture;
  }
}

export function postureChipClass(posture: KarakaraPosture): string {
  switch (posture) {
    case "off":
      return "karakara-posture-chip karakara-posture-chip--off";
    case "audit":
      return "karakara-posture-chip karakara-posture-chip--audit";
    case "enforce":
      return "karakara-posture-chip karakara-posture-chip--enforce";
    default:
      return "karakara-posture-chip";
  }
}

export function incidentSeverityClass(severity: string): string {
  switch (severity) {
    case "critical":
      return "karakara-incident--critical";
    case "warning":
      return "karakara-incident--warning";
    default:
      return "karakara-incident--info";
  }
}

export function formatIncidentTime(ms: number): string {
  if (!ms) return "—";
  try {
    return new Date(ms).toLocaleString();
  } catch {
    return String(ms);
  }
}

export { preventionRatioLabel } from "../utils/gateHelpers";
