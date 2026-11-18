# Rootfs apply step (U4)

`RootfsApplyStep.vue` is the S6/S7 body for U3's step 3. U3 owns the enclosing
dialog, its title and the step footer. Render the component only after the S5
confirmation has succeeded:

```vue
<RootfsApplyStep
  :open="step === 3"
  :plan-id="plan.plan_id"
  @close="closeDialog"
  @check-again="showRootfsReport"
  @complete="refreshHistory"
/>
```

- `open` starts or resumes following the confirmed plan. Pass `job-id` and
  `follow-only` when the global job strip opens an existing apply.
- The component submits `rootfs.apply { plan_id, confirm_no_restore: true }`,
  follows the matching `rootfs-apply` entry in `jobs.list`, and reads the final
  document through `job.result`.
- Closing or unmounting the component never cancels the detached job. Its job
  id is kept in session storage; the application job store continues polling.
- **Stop after this batch** calls `job.cancel`. The component remains on S6 until
  the job result says `completion: paused`; the same plan can then be continued.
- `check-again` carries the newly read `rootfs.report` (or `null` on failure).
  U3 should leave step 3 and show its S3 view with that report.
- `complete` carries the `rootfs-apply` result so U3 can refresh surrounding
  state. Nothing in this component mutates History.

`RootfsHistorySection.vue` is mounted by History with the `rootfs.history`
records and the current partition state. Its `check-and-clean-up` event returns
to Overview and selects the System partition card. The read-only detail uses
only A6 fields. A6 currently has a skipped count, not the skipped path/reason
list or category totals, so those per-file details cannot be reconstructed in
History; the apply result screen shows localized skipped counts by reason.

The types in `rootfsTypes.ts` are hand-written until the rootfs WebAPI has JSON
schemas. The dev-only page is `/rootfs-apply-fixture.html`; it selects scenarios
through `?scenario=<name>` and is not part of the DSM production entry point.
