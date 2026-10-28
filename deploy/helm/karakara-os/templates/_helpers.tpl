{{/*
Chart name, or nameOverride if set.
*/}}
{{- define "karakara-os.name" -}}
{{- .Values.nameOverride | default .Chart.Name | trunc 63 | trimSuffix "-" }}
{{- end }}

{{/*
Release-qualified chart name, or fullnameOverride if set.
Truncated to 45 chars so component suffixes (-runner, -surrealdb)
and terminal suffixes (-api, -ingress) stay unique and within
the 63-char Kubernetes name limit.
*/}}
{{- define "karakara-os.fullname" -}}
{{- if .Values.fullnameOverride }}
{{- .Values.fullnameOverride | trunc 45 | trimSuffix "-" }}
{{- else }}
{{- printf "%s-%s" .Release.Name (include "karakara-os.name" .) | trunc 45 | trimSuffix "-" }}
{{- end }}
{{- end }}

{{/*
Runner resource name (fullname + "-runner", max 52 chars).
*/}}
{{- define "karakara-os.runner.fullname" -}}
{{- printf "%s-runner" (include "karakara-os.fullname" .) }}
{{- end }}

{{/*
SurrealDB resource name (fullname + "-surrealdb", max 55 chars).
*/}}
{{- define "karakara-os.surrealdb.fullname" -}}
{{- printf "%s-surrealdb" (include "karakara-os.fullname" .) }}
{{- end }}

{{/*
Chart label value (name-version).
*/}}
{{- define "karakara-os.chart" -}}
{{- printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" | trunc 63 | trimSuffix "-" }}
{{- end }}

{{/*
Common labels applied to every resource.
instance lives in selectorLabels (not here) to stay out of the
immutable StatefulSet selector matchLabels set.
*/}}
{{- define "karakara-os.labels" -}}
helm.sh/chart: {{ include "karakara-os.chart" . }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
{{- if .Values.global.environment }}
app.kubernetes.io/environment: {{ .Values.global.environment }}
{{- end }}
{{- end }}

{{/*
Runner selector labels — immutable across upgrades.
Used by: runner StatefulSet selector, runner headless Service selector,
runner API Service selector, NetworkPolicy runner pod selectors.
*/}}
{{- define "karakara-os.runner.selectorLabels" -}}
app.kubernetes.io/name: {{ include "karakara-os.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
app.kubernetes.io/component: runner
{{- end }}

{{/*
Full runner labels (common + selector).
*/}}
{{- define "karakara-os.runner.labels" -}}
{{ include "karakara-os.labels" . }}
{{ include "karakara-os.runner.selectorLabels" . }}
{{- end }}

{{/*
SurrealDB selector labels — immutable across upgrades.
Used by: SurrealDB StatefulSet selector, SurrealDB Service selector,
NetworkPolicy surrealdb pod selectors.
*/}}
{{- define "karakara-os.surrealdb.selectorLabels" -}}
app.kubernetes.io/name: {{ include "karakara-os.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
app.kubernetes.io/component: surrealdb
{{- end }}

{{/*
Full SurrealDB labels (common + selector).
*/}}
{{- define "karakara-os.surrealdb.labels" -}}
{{ include "karakara-os.labels" . }}
{{ include "karakara-os.surrealdb.selectorLabels" . }}
{{- end }}
