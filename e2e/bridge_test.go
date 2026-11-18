// Package e2e serves Reclaim's App through the SDK's Go runtime, as the
// package CGI does, with the SDK's fake executor on the runtime's executor socket.
package e2e

import (
	"context"
	"crypto/sha256"
	"encoding/json"
	"fmt"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"reflect"
	"strings"
	"testing"

	"github.com/synopackageland/syno-pkg-land-sdk/api"
	"github.com/synopackageland/syno-pkg-land-sdk/bridge"
	"github.com/synopackageland/syno-pkg-land-sdk/delegate"
	"github.com/synopackageland/syno-pkg-land-sdk/delegate/delegatetest"
)

// The package ID the tests run as.
const pkg = "SynoReclaimE2E"

type admin struct{}

func (admin) Authenticate(*http.Request) (api.Identity, error) {
	return api.Identity{User: "admin", Admin: true}, nil
}

// cliRun answers a root.run of the root program with document and exit code 0.
func cliRun(document string) string {
	text, _ := json.Marshal(document)
	return delegatetest.OK(`{"request_id":"r1","exit_code":0,"stderr":"","output":` + string(text) + `}`)
}

// runtime is the package CGI for the built App, its bridge.json as the
// CLI writes it from dsm-package.yaml, with e on its executor socket.
func runtime(t *testing.T, e *delegatetest.Executor) http.Handler {
	return runtimeAs(t, e, admin{})
}

func runtimeAs(t *testing.T, e *delegatetest.Executor, auth api.Authenticator) http.Handler {
	t.Helper()
	app := os.Getenv("RECLAIM_APP")
	if app == "" {
		t.Skip("RECLAIM_APP names the built synoreclaim-app; run e2e/run.sh")
	}
	var manifest struct {
		Portal  map[string]any `json:"portal"`
		Backend struct {
			Methods        []string `json:"methods"`
			TimeoutSeconds int      `json:"timeout_seconds"`
		} `json:"backend"`
	}
	raw, err := os.ReadFile("../dsm-package.yaml")
	if err != nil {
		t.Fatal(err)
	}
	if err := json.Unmarshal(raw, &manifest); err != nil {
		t.Fatal(err)
	}
	bridgeJSON, _ := json.Marshal(map[string]any{
		"schema": 1, "runtime": "rust", "entry": "bin/synoreclaim-app", "mode": "exec",
		"methods": manifest.Backend.Methods, "timeout_seconds": manifest.Backend.TimeoutSeconds, "access": "admin-only",
		"capabilities": map[string]bool{"root": true, "portal": manifest.Portal != nil},
	})
	dir := t.TempDir()
	permissions, err := os.ReadFile("../app/permissions.json")
	if err != nil {
		t.Fatal(err)
	}
	os.WriteFile(filepath.Join(dir, bridge.ManifestFile), bridgeJSON, 0o600)
	os.WriteFile(filepath.Join(dir, bridge.PermissionsFile), permissions, 0o600)
	operations, err := os.ReadFile("../app/synopkgland-operations.json")
	if err != nil {
		t.Fatal(err)
	}
	os.WriteFile(filepath.Join(dir, "synopkgland-operations.json"), operations, 0o600)
	os.MkdirAll(filepath.Join(dir, "bin"), 0o700)
	if err := os.Symlink(app, filepath.Join(dir, "bin/synoreclaim-app")); err != nil {
		t.Fatal(err)
	}
	h, err := bridge.NewRuntime(bridge.Config{
		PackageID: pkg, AppDir: dir, DataDir: t.TempDir(), Auth: auth, ExecutorSocket: e.Socket, PortalExecutor: true,
		OperationsDigest: fmt.Sprintf("%x", sha256.Sum256(operations)),
		ManifestDigest:   fmt.Sprintf("%x", sha256.Sum256(bridgeJSON)), PermissionsDigest: fmt.Sprintf("%x", sha256.Sum256(permissions)),
	})
	if err != nil {
		t.Fatal(err)
	}
	return h
}

type normalUser struct{}

func (normalUser) Authenticate(*http.Request) (api.Identity, error) {
	return api.Identity{User: "normal1"}, nil
}

type noSession struct{}

func (noSession) Authenticate(*http.Request) (api.Identity, error) {
	return api.Identity{}, api.ErrNoSession
}

func TestPhoneEntryStatusNeedsAnAdministratorWithoutExecutor(t *testing.T) {
	e := delegatetest.NewExecutor(t, func(delegate.Request) string {
		t.Error("phone-entry metadata reached the executor")
		return ""
	})
	for _, tc := range []struct {
		name string
		auth api.Authenticator
		code string
	}{
		{"anonymous", noSession{}, "session_required"},
		{"ordinary", normalUser{}, "admin_required"},
		{"administrator", admin{}, ""},
	} {
		t.Run(tc.name, func(t *testing.T) {
			got := call(t, runtimeAs(t, e, tc.auth), `{"method":"app.status","params":{}}`)
			if tc.code != "" {
				if got["success"] != false || got["error"].(map[string]any)["code"] != tc.code || got["data"] != nil {
					t.Fatalf("entry refusal: %v", got)
				}
				return
			}
			status := data(t, got)
			raw, err := os.ReadFile("../dsm-package.yaml")
			if err != nil {
				t.Fatal(err)
			}
			var manifest struct {
				Version string `json:"version"`
				Build   int    `json:"build"`
			}
			if err := json.Unmarshal(raw, &manifest); err != nil {
				t.Fatal(err)
			}
			want := map[string]any{
				"package": "SynoReclaim", "version": fmt.Sprintf("%s-%d", manifest.Version, manifest.Build),
				"user": "admin", "admin": true, "lang": "", "locale": "",
			}
			if !reflect.DeepEqual(status, want) {
				t.Fatalf("entry metadata: %v", status)
			}
		})
	}
	if len(e.Requests()) != 0 {
		t.Fatal("phone entry invoked a root operation")
	}
}

// run.sh links the same policy digest as api.cgi. Formatting compatibility
// must not be obtained by disabling the check that rejects modified policy.
func TestBuildBoundPermissionsRejectTampering(t *testing.T) {
	raw, err := os.ReadFile("../app/permissions.json")
	if err != nil {
		t.Fatal(err)
	}
	var policy struct {
		Methods map[string]string `json:"methods"`
	}
	if err := json.Unmarshal(raw, &policy); err != nil {
		t.Fatal(err)
	}
	routes := make([]api.Route, 0, len(policy.Methods))
	for method := range policy.Methods {
		routes = append(routes, api.Handle(method, func(_ context.Context, _ api.Caller, _ json.RawMessage) (any, error) {
			return nil, nil
		}))
	}
	if _, err := api.NewServerWithPermissions(admin{}, raw, routes...); err != nil {
		t.Fatal(err)
	}
	if _, err := api.NewServerWithPermissions(admin{}, append(raw, '\n'), routes...); err == nil || !strings.Contains(err.Error(), "App permissions do not match package build policy") {
		t.Fatalf("changed policy must fail its build digest (run with e2e/run.sh): %v", err)
	}
}

func TestBuildBoundRuntimeStarts(t *testing.T) {
	e := delegatetest.NewExecutor(t, func(delegate.Request) string {
		t.Fatal("runtime startup must not invoke the executor")
		return ""
	})
	runtime(t, e)
}

func TestCtlBusyScanReturnsExistingIDWithoutStartingAnotherJob(t *testing.T) {
	e := delegatetest.NewExecutor(t, func(q delegate.Request) string {
		if q.Op != "root.run" {
			t.Fatal("busy scan launched another job")
		}
		return delegatetest.OK(`{"exit_code":4,"output":"{\"command\":\"detach\",\"outcome\":\"scan_in_progress\",\"running_job_id\":\"j-existing\"}"}`)
	})
	got := data(t, call(t, runtime(t, e), `{"method":"scans.start","params":{"targets":["/volume1/data"],"mode":"gentle"}}`))
	if got["accepted"] != false || got["outcome"] != "scan_in_progress" || got["running_job_id"] != "j-existing" || len(e.Requests()) != 1 {
		t.Fatalf("busy: %v", got)
	}
}

func TestCtlScanFlowKeepsTheSameScanAndPartialResult(t *testing.T) {
	e := delegatetest.NewExecutor(t, func(q delegate.Request) string {
		if q.Op == "root.start" {
			return delegatetest.OK(`{"job_id":"rj-ctl"}`)
		}
		switch params(t, q)["params"].(map[string]any)["method"] {
		case "scan.start":
			return cliRun(`{"command":"detach","job_id":"j-ctl"}`)
		case "jobs.list":
			return cliRun(`{"jobs":[{"job_id":"j-other","kind":"scan","state":"completed"},{"job_id":"j-ctl","kind":"scan","state":"partial","targets":[{"path":"/volume1/example"}],"profile":"low","requested_by":"admin"}]}`)
		case "job.result":
			if params(t, q)["params"].(map[string]any)["params"] != `{"job_id":"j-ctl"}` {
				t.Fatal("result lost the original scan identity")
			}
			return cliRun(`{"command":"scan","job_id":"j-ctl","completion":"partial"}`)
		default:
			t.Fatal("unexpected operation")
			return ""
		}
	})
	h := runtime(t, e)
	accepted := data(t, call(t, h, `{"method":"scans.start","params":{"targets":["/volume1/example"],"mode":"gentle","snapshots":false}}`))
	if accepted["job_id"] != "j-ctl" {
		t.Fatalf("acceptance %v", accepted)
	}
	status := data(t, call(t, h, `{"method":"scans.status","params":{"job_id":"j-ctl"}}`))
	if status["job_id"] != "j-ctl" || status["state"] != "partial" || status["profile"] != "low" || status["target_count"] != 1.0 {
		t.Fatalf("status %v", status)
	}
	result := data(t, call(t, h, `{"method":"ctl.job.result","params":{"job_id":"j-ctl"}}`))
	if result["completion"] != "partial" || len(e.Requests()) != 4 {
		t.Fatalf("result %v, calls %d", result, len(e.Requests()))
	}
}

func TestCtlMethodsKeepTheAdministratorEntryFloor(t *testing.T) {
	e := delegatetest.NewExecutor(t, func(delegate.Request) string {
		t.Error("nonadministrator reached executor")
		return ""
	})
	h := runtimeAs(t, e, normalUser{})
	for _, method := range []string{"scans.start", "scans.status", "ctl.volumes.list", "ctl.job.result", "cleanup.plan", "rootfs.plan", "ctl.cleanup.plan", "ctl.rootfs.plan", "ctl.cleanup.apply", "ctl.rootfs.apply", "ctl.job.cancel", "ctl.setup.cleanup", "ctl.cache.clear"} {
		got := call(t, h, `{"method":"`+method+`","params":{}}`)
		if got["success"] != false || got["error"].(map[string]any)["code"] != "admin_required" {
			t.Fatalf("%s: %v", method, got)
		}
	}
	if len(e.Requests()) != 0 {
		t.Fatal("nonadministrator invoked a root operation")
	}
}

func TestCtlReadDoesNotEchoPrivatePathsOrFutureBackendFields(t *testing.T) {
	private := "private-url-token-passkey-file-content"
	e := delegatetest.NewExecutor(t, func(q delegate.Request) string {
		inner := params(t, q)["params"].(map[string]any)
		if inner["method"] != "top.list" || !strings.Contains(inner["params"].(string), private) {
			t.Error("private input did not reach the original validated method")
		}
		return cliRun(`{"completion":"partial","results":[{"path":"` + private + `","metrics":{"logical":456,"reclaimable":123,"token":"` + private + `"},"new_numeric_field":42}],"message":"` + private + `"}`)
	})
	got := data(t, call(t, runtime(t, e), `{"method":"ctl.top.list","params":{"target":"/volume1/`+private+`","kind":"all"}}`))
	raw, _ := json.Marshal(got)
	if strings.Contains(string(raw), private) || strings.Contains(string(raw), "new_numeric_field") {
		t.Fatal("ctl response exposed private data or an unrecognized field")
	}
	if got["completion"] != "partial" || got["results"].([]any)[0].(map[string]any)["metrics"].(map[string]any)["reclaimable"] != 123.0 {
		t.Fatalf("projection lost coverage or statistics: %v", got)
	}
}

// call posts a request as DSM's window does, with its session.
func call(t *testing.T, h http.Handler, body string) map[string]any {
	t.Helper()
	r := httptest.NewRequest("POST", "https://nas.local:5001/webman/3rdparty/SynoReclaim/cgi/api.cgi", strings.NewReader(body))
	r.Header.Set("Content-Type", "application/json")
	r.Header.Set("Origin", "https://nas.local:5001")
	r.Header.Set("Cookie", "id=session-id")
	r.Header.Set("X-SYNO-TOKEN", "synotoken")
	w := httptest.NewRecorder()
	h.ServeHTTP(w, r)
	var envelope map[string]any
	if err := json.Unmarshal(w.Body.Bytes(), &envelope); err != nil {
		t.Fatalf("%s: %v", w.Body.String(), err)
	}
	return envelope
}

func data(t *testing.T, envelope map[string]any) map[string]any {
	t.Helper()
	if envelope["success"] != true {
		t.Fatalf("refused: %v", envelope)
	}
	return envelope["data"].(map[string]any)
}

func params(t *testing.T, q delegate.Request) map[string]any {
	t.Helper()
	var p map[string]any
	if err := json.Unmarshal(q.Params, &p); err != nil {
		t.Fatal(err)
	}
	return p
}

func TestAMethodRunsTheRootProgramWithTheCallersSession(t *testing.T) {
	e := delegatetest.NewExecutor(t, func(delegate.Request) string { return cliRun(`{"command":"volumes.list","volumes":[]}`) })
	got := data(t, call(t, runtime(t, e), `{"method":"volumes.list","params":{"all":true}}`))
	if got["command"] != "volumes.list" || got["exit"] != 0.0 {
		t.Fatalf("data %v", got)
	}
	seen := e.Requests()
	if len(seen) != 1 || seen[0].Op != "root.run" {
		t.Fatalf("requests %+v", seen)
	}
	if seen[0].Auth["dsm_cookie"] != "session-id" || seen[0].Auth["dsm_synotoken"] != "synotoken" {
		t.Fatalf("the runtime passes the session on: %+v", seen[0].Auth)
	}
	p := params(t, seen[0])
	inner := p["params"].(map[string]any)
	if p["name"] != "cli" || inner["method"] != "volumes.list" || inner["params"] != `{"all":true}` {
		t.Fatalf("root.run params %v", p)
	}
}

func TestAQueuedScanStartsAsARootJobThatACancelStops(t *testing.T) {
	e := delegatetest.NewExecutor(t, func(q delegate.Request) string {
		switch q.Op {
		case "root.start":
			return delegatetest.OK(`{"job_id":"rj-1"}`)
		case "root.jobs.stop":
			return delegatetest.OK(`{"job_id":"rj-1","state":"stopping"}`)
		}
		method := params(t, q)["params"].(map[string]any)["method"]
		if method == "scan.start" {
			return cliRun(`{"command":"detach","job_id":"j1"}`)
		}
		return cliRun(`{"command":"job.cancel","job_id":"j1"}`)
	})
	h := runtime(t, e)
	if got := data(t, call(t, h, `{"method":"scan.start","params":{"volume":"/volume1"}}`)); got["job_id"] != "j1" {
		t.Fatalf("scan %v", got)
	}
	data(t, call(t, h, `{"method":"job.cancel","params":{"job_id":"j1"}}`))
	var ops []string
	for _, q := range e.Requests() {
		ops = append(ops, q.Op)
	}
	if strings.Join(ops, " ") != "root.run root.start root.run root.jobs.stop" {
		t.Fatalf("ops %v", ops)
	}
	seen := e.Requests()
	if start := params(t, seen[1]); start["name"] != "cli" || start["params"].(map[string]any)["job_id"] != "j1" {
		t.Fatalf("root.start %v", start)
	}
	if stop := params(t, seen[3]); stop["job_id"] != "rj-1" {
		t.Fatalf("root.jobs.stop %v", stop)
	}
}

func TestTheExecutorsRefusalReachesTheWindow(t *testing.T) {
	e := delegatetest.NewExecutor(t, func(delegate.Request) string { return delegatetest.Refused("EXECUTOR_PAUSED") })
	got := call(t, runtime(t, e), `{"method":"overview.get","params":{}}`)
	if got["success"] != false || got["error"].(map[string]any)["code"] != "EXECUTOR_PAUSED" {
		t.Fatalf("envelope %v", got)
	}
}

func TestTheApprovalStatusAsksTheExecutor(t *testing.T) {
	for reply, want := range map[string]string{
		delegatetest.OK(`{"jobs":[]}`):          "ready",
		delegatetest.Refused("ADMIN_REQUIRED"):  "not_admin",
		delegatetest.Refused("EXECUTOR_PAUSED"): "authorization_required",
	} {
		e := delegatetest.NewExecutor(t, func(delegate.Request) string { return reply })
		got := data(t, call(t, runtime(t, e), `{"method":"authorization.status","params":{}}`))
		if got["state"] != want || e.Requests()[0].Op != "root.jobs.list" {
			t.Fatalf("%s: %v", reply, got)
		}
	}
}

// Approval adds no operation to the policy: the status call fails with the
// refusal instead of asking for approval again.
func TestTheApprovalStatusFailsOnAPolicyRefusal(t *testing.T) {
	e := delegatetest.NewExecutor(t, func(delegate.Request) string { return delegatetest.Refused("POLICY_DENIED") })
	got := call(t, runtime(t, e), `{"method":"authorization.status","params":{}}`)
	if got["success"] != false || got["error"].(map[string]any)["code"] != "POLICY_DENIED" {
		t.Fatalf("envelope %v", got)
	}
}

// The CLI's root window starts the approved executor and checks its pins
// through these; the runtime serves them, not the App.
func TestTheWindowsExecutorRoutesReachTheRuntime(t *testing.T) {
	e := delegatetest.NewExecutor(t, func(q delegate.Request) string {
		if q.Op != "status" {
			return delegatetest.Refused(delegate.CodeUnknownOp)
		}
		return delegatetest.OK(`{"ready":true,"state":"ready","executor_digest":"e1","policy_digest":"p1"}`)
	})
	h := runtime(t, e)
	if got := data(t, call(t, h, `{"method":"executor.control","params":{"action":"start"}}`)); got["written"] != true {
		t.Fatalf("control %v", got)
	}
	if got := data(t, call(t, h, `{"method":"executor.status","params":{}}`)); got["executor_digest"] != "e1" || got["policy_digest"] != "p1" {
		t.Fatalf("status %v", got)
	}
}

// Settings must read the executor's entry, including incomplete cleanup. An
// unreachable executor must never be presented as a registered entry.
func TestPortalSettingsReadsRegistrationState(t *testing.T) {
	for _, entry := range []string{"registered", "unregistered", "cleanup-required", "unavailable"} {
		t.Run(entry, func(t *testing.T) {
			e := delegatetest.NewExecutor(t, func(q delegate.Request) string {
				if q.Op != "status" {
					t.Errorf("unexpected operation %s", q.Op)
				}
				if entry == "unavailable" {
					return delegatetest.Refused("EXECUTOR_UNAVAILABLE")
				}
				return delegatetest.OK(`{"portal":"` + entry + `"}`)
			})
			got := data(t, call(t, runtime(t, e), `{"method":"portal.status","params":{}}`))
			if entry == "unavailable" {
				if got["state"] != "unavailable" || got["entry"] == "registered" {
					t.Fatalf("unreachable executor: %v", got)
				}
			} else if got["entry"] != entry {
				t.Fatalf("want entry %s, got %v", entry, got)
			}
			if len(e.Requests()) != 1 || len(e.Requests()[0].Auth) != 0 {
				t.Fatalf("status must read the executor without session credentials: %+v", e.Requests())
			}
		})
	}
}

func TestPortalSettingsChangesEntryThroughTheBridge(t *testing.T) {
	for op, entry := range map[string]string{"portal.register": "registered", "portal.unregister": "unregistered"} {
		t.Run(op, func(t *testing.T) {
			e := delegatetest.NewExecutor(t, func(q delegate.Request) string {
				if q.Op == op {
					return delegatetest.OK(`{"entry":"` + entry + `"}`)
				}
				return delegatetest.OK(`{"portal":"` + entry + `"}`)
			})
			got := data(t, call(t, runtime(t, e), `{"method":"`+op+`","params":{}}`))
			if got["entry"] != entry {
				t.Fatalf("entry after %s: %v", op, got)
			}
			seen := e.Requests()
			if len(seen) != 2 || seen[0].Op != op || seen[1].Op != "status" {
				t.Fatalf("mutation followed by readback: %+v", seen)
			}
			if seen[0].Auth["dsm_cookie"] != "session-id" || seen[0].Auth["dsm_synotoken"] != "synotoken" || len(params(t, seen[0])) != 0 {
				t.Fatalf("fixed operation must carry the administrator session: %+v", seen[0])
			}
		})
	}
}

func TestPlanningUsesExistingRootStartAndSavedProductResult(t *testing.T) {
	for _, method := range []string{"cleanup.plan", "rootfs.plan", "ctl.cleanup.plan", "ctl.rootfs.plan", "rootfs.report", "ctl.rootfs.report"} {
		t.Run(method, func(t *testing.T) {
			backend := strings.TrimPrefix(method, "ctl.")
			kind := strings.ReplaceAll(backend, ".", "-")
			phase := "plan"
			if backend == "rootfs.report" {
				phase = "report"
			}
			e := delegatetest.NewExecutor(t, func(q delegate.Request) string {
				if q.Op == "root.start" {
					return delegatetest.OK(`{"job_id":"root-plan"}`)
				}
				inner := params(t, q)["params"].(map[string]any)
				switch inner["method"] {
				case backend:
					return cliRun(`{"command":"detach","job_id":"plan-123","run":["private-launch"],"result":{"path":"private-result"}}`)
				case "jobs.list":
					return cliRun(`{"jobs":[{"job_id":"plan-123","kind":"` + kind + `","state":"running","phase":"` + phase + `","elapsed_s":99}]}`)
				case "job.result":
					if backend == "rootfs.report" {
						return cliRun(`{"command":"rootfs","completion":"complete","filesystem":{"free_bytes":42},"owners":[{"name":"private-owner","bytes":13}]}`)
					}
					if backend == "cleanup.plan" {
						return cliRun(`{"command":"cleanup-plan","completion":"complete","permanent":true,"plan":{"plan_id":"cleanup-aa-bb","expires_at":123,"estimate":{"reclaimable":42},"targets":[{"path":"private-target"}]}}`)
					}
					return cliRun(`{"command":"rootfs-plan","completion":"complete","plan_id":"rootfs-aa-bb","moves":{"bytes":42},"destination":{"path":"private-target"}}`)
				default:
					t.Fatal("unexpected planning operation")
					return ""
				}
			})
			h := runtime(t, e)
			started := data(t, call(t, h, `{"method":"`+method+`","params":{}}`))
			if started["job_id"] != "plan-123" || len(e.Requests()) != 2 {
				t.Fatalf("start waited or lost identity: %v", started)
			}
			status := data(t, call(t, h, `{"method":"ctl.jobs.list","params":{}}`))
			job := status["jobs"].([]any)[0].(map[string]any)
			if job["kind"] != kind || job["phase"] != phase || job["elapsed_s"] != 99.0 {
				t.Fatalf("planning status %v", job)
			}
			result := data(t, call(t, h, `{"method":"ctl.job.result","params":{"job_id":"plan-123"}}`))
			raw, _ := json.Marshal(result)
			if strings.Contains(string(raw), "private-") || result["completion"] != "complete" {
				t.Fatalf("unsafe or incomplete result %s", raw)
			}
			if backend == "cleanup.plan" && result["plan"].(map[string]any)["plan_id"] != "cleanup-aa-bb" {
				t.Fatalf("plan identity missing: %v", result)
			}
			if backend == "rootfs.report" && result["filesystem"].(map[string]any)["free_bytes"] != 42.0 {
				t.Fatalf("report metrics missing: %v", result)
			}
			if backend == "rootfs.plan" && result["plan_id"] != "rootfs-aa-bb" {
				t.Fatalf("plan identity missing: %v", result)
			}
		})
	}
}

func TestDeepRootfsResultsSurviveRuntimeBridge(t *testing.T) {
	for _, kind := range []string{"rootfs.report", "rootfs.plan"} {
		t.Run(kind, func(t *testing.T) {
			raw, err := os.ReadFile("../ui/fixtures/" + kind + "/deep-short.json")
			if err != nil {
				t.Fatal(err)
			}
			var expected map[string]any
			if err := json.Unmarshal(raw, &expected); err != nil {
				t.Fatal(err)
			}
			e := delegatetest.NewExecutor(t, func(q delegate.Request) string { return cliRun(string(raw)) })
			h := runtime(t, e)
			for _, method := range []string{"job.result", "ctl.job.result"} {
				got := data(t, call(t, h, `{"method":"`+method+`","params":{"job_id":"plan-123"}}`))
				if kind == "rootfs.report" {
					if !reflect.DeepEqual(got["cleanable"], expected["cleanable"]) {
						t.Fatalf("%s dropped Deep accounting: %v", method, got)
					}
				} else {
					if !reflect.DeepEqual(got["moves"], expected["moves"]) || !reflect.DeepEqual(got["follow_up"], expected["follow_up"]) {
						t.Fatalf("%s dropped plan fields: %v", method, got)
					}
					tool := got["tools"].([]any)[0].(map[string]any)
					if tool["tool"] != "delete_log" || tool["estimated_bytes"] == nil {
						t.Fatalf("%s dropped delete_log: %v", method, tool)
					}
					if method == "ctl.job.result" && tool["argv"] != nil {
						t.Fatal("ctl exposed tool argv")
					}
				}
			}
		})
	}
}

func TestRegistryRejectsMissingExtraAndMismatchedPermissions(t *testing.T) {
	raw, err := os.ReadFile("../operations.json")
	if err != nil {
		t.Fatal(err)
	}
	permissions, err := os.ReadFile("../app/permissions.json")
	if err != nil {
		t.Fatal(err)
	}
	var registry struct {
		Format     string                     `json:"format"`
		Version    int                        `json:"version"`
		Operations map[string]json.RawMessage `json:"operations"`
	}
	if err := json.Unmarshal(raw, &registry); err != nil {
		t.Fatal(err)
	}
	routes := make([]api.Route, 0, len(registry.Operations))
	for method := range registry.Operations {
		routes = append(routes, api.Handle(method, func(context.Context, api.Caller, json.RawMessage) (any, error) { return nil, nil }))
	}
	if _, err := api.NewServerWithOperations(admin{}, raw, permissions, routes...); err != nil {
		t.Fatal(err)
	}
	for _, kind := range []string{"missing", "extra", "permission"} {
		t.Run(kind, func(t *testing.T) {
			var mutated map[string]any
			json.Unmarshal(raw, &mutated)
			ops := mutated["operations"].(map[string]any)
			switch kind {
			case "missing":
				delete(ops, "ctl.cleanup.apply")
			case "extra":
				ops["unexpected.route"] = ops["ctl.cleanup.apply"]
			case "permission":
				ops["ctl.cleanup.apply"].(map[string]any)["permission"] = "public"
			}
			changed, _ := json.Marshal(mutated)
			if _, err := api.NewServerWithOperations(admin{}, changed, permissions, routes...); err == nil {
				t.Fatal("registry drift accepted")
			}
		})
	}
}
