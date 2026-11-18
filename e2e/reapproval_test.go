package e2e

import (
	"sync/atomic"
	"testing"

	"github.com/synopackageland/syno-pkg-land-sdk/delegate"
	"github.com/synopackageland/syno-pkg-land-sdk/delegate/delegatetest"
)

// Simulates an upgrade's re-approval state through the existing executor hook.
// This is runtime/ctl-binding coverage, not a package upgrade or a NAS CLI test.
func TestSimulatedUpgradeReapprovalThenCtlResumeKeepsPausedScanIdentity(t *testing.T) {
	var approved atomic.Bool
	var resumed atomic.Bool
	var starts atomic.Int32
	e := delegatetest.NewExecutor(t, func(q delegate.Request) string {
		if !approved.Load() {
			return delegatetest.Refused("EXECUTOR_PAUSED")
		}
		switch q.Op {
		case "root.jobs.list":
			return delegatetest.OK(`{"jobs":[]}`)
		case "root.start":
			if params(t, q)["params"].(map[string]any)["job_id"] != "j-paused" {
				t.Fatal("resume started another scan identity")
			}
			starts.Add(1)
			return delegatetest.OK(`{"job_id":"rj-resumed"}`)
		}
		input := params(t, q)["params"].(map[string]any)
		switch input["method"] {
		case "job.resume":
			if input["params"] != `{"job_id":"j-paused","mode":"gentle"}` {
				t.Fatalf("resume parameters changed: %v", input["params"])
			}
			resumed.Store(true)
			return cliRun(`{"command":"detach","job_id":"j-paused"}`)
		case "jobs.list":
			state := "paused"
			if resumed.Load() {
				state = "completed"
			}
			return cliRun(`{"jobs":[{"job_id":"j-paused","kind":"scan","state":"` + state + `"}]}`)
		case "job.result":
			return cliRun(`{"command":"scan","job_id":"j-paused","completion":"complete","entries":3}`)
		default:
			t.Fatalf("unexpected method: %v", input["method"])
			return ""
		}
	})
	h := runtime(t, e)
	status := data(t, call(t, h, `{"method":"ctl.authorization.status","params":{}}`))
	if status["state"] != "authorization_required" || status["reason"] != "upgrade_reapproval" {
		t.Fatalf("re-approval status: %v", status)
	}
	refused := call(t, h, `{"method":"ctl.job.resume","params":{"job_id":"j-paused","mode":"gentle"}}`)
	if refused["success"] != false || starts.Load() != 0 || resumed.Load() {
		t.Fatalf("unapproved resume started work: %v", refused)
	}
	// Existing delegatetest hook stands in for the protected DSM password approval.
	approved.Store(true)
	status = data(t, call(t, h, `{"method":"ctl.authorization.status","params":{}}`))
	if status["state"] != "ready" {
		t.Fatalf("approved status: %v", status)
	}
	accepted := data(t, call(t, h, `{"method":"ctl.job.resume","params":{"job_id":"j-paused","mode":"gentle"}}`))
	if accepted["accepted"] != true || accepted["job_id"] != "j-paused" || starts.Load() != 1 {
		t.Fatalf("resume admission: %v, starts %d", accepted, starts.Load())
	}
	status = data(t, call(t, h, `{"method":"scans.status","params":{"job_id":"j-paused"}}`))
	if status["job_id"] != "j-paused" || status["state"] != "completed" {
		t.Fatalf("resumed status: %v", status)
	}
	result := data(t, call(t, h, `{"method":"ctl.job.result","params":{"job_id":"j-paused"}}`))
	if result["job_id"] != "j-paused" || result["completion"] != "complete" {
		t.Fatalf("resumed result: %v", result)
	}
}
