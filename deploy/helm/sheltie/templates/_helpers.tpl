{{- define "sheltie.fullname" -}}
{{- if contains .Chart.Name .Release.Name -}}
{{- .Release.Name | trunc 63 | trimSuffix "-" -}}
{{- else -}}
{{- printf "%s-%s" .Release.Name .Chart.Name | trunc 63 | trimSuffix "-" -}}
{{- end -}}
{{- end -}}

{{- define "sheltie.labels" -}}
app.kubernetes.io/name: {{ .Chart.Name }}
app.kubernetes.io/instance: {{ .Release.Name }}
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
helm.sh/chart: {{ printf "%s-%s" .Chart.Name .Chart.Version }}
{{- end -}}

{{- define "sheltie.selectorLabels" -}}
app.kubernetes.io/name: {{ .Chart.Name }}
app.kubernetes.io/instance: {{ .Release.Name }}
{{- end -}}

{{- define "sheltie.publicHosts" -}}
{{- if .Values.publicHosts -}}
{{- join "," .Values.publicHosts -}}
{{- else if .Values.ingress.enabled -}}
{{- $hosts := list -}}
{{- range .Values.ingress.hosts }}{{ $hosts = append $hosts .host }}{{ end -}}
{{- join "," $hosts -}}
{{- end -}}
{{- end -}}
