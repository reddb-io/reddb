import { describe, expect, it } from "vitest";
import TaskList from "../src/composites/TaskList.svelte";
import { validateTasks } from "../src/composites/task-list.behavior";
import { render } from "./mount";

describe("TaskList",()=>{
  it("keeps task identity, localized status, blockers and progress semantics",()=>{
    const root=render(TaskList,{label:"Operations",items:[
      {id:"import",title:"Customer import",state:"blocked",stateLabel:"Aguardando capacidade",progress:18,worker:"Worker 3",reason:"Free 2 GB to continue.",href:"/imports/3"},
      {id:"verify",title:"Verify snapshot",state:"running",progress:null,estimate:"Estimating remaining time"},
      {id:"queue",title:"Next backup",state:"queued"},
    ]});
    expect(root.querySelector('ul')?.getAttribute("aria-label")).toBe("Operations");
    const task=root.querySelector('[data-task-id="import"]')!;
    expect(task.textContent).toContain("Aguardando capacidade");expect(task.textContent).toContain("Free 2 GB to continue.");
    expect(task.querySelector("a")?.getAttribute("href")).toBe("/imports/3");
    expect(task.querySelector('[role="progressbar"]')?.getAttribute("aria-valuenow")).toBe("18");
    expect(root.querySelector('[data-task-id="verify"] [role="progressbar"]')?.hasAttribute("aria-valuenow")).toBe(false);
    expect(root.querySelector('[data-task-id="queue"] [role="progressbar"]')).toBeNull();
  });
  it("explains an empty queue and refuses ambiguous identities",()=>{
    expect(render(TaskList,{label:"Jobs",items:[],emptyLabel:"No work pending",emptyDescription:"Start an import to add a task."}).textContent).toContain("Start an import to add a task.");
    expect(()=>validateTasks([{id:"same",title:"A",state:"queued"},{id:"same",title:"B",state:"running"}])).toThrow("unique");
    expect(()=>validateTasks([{id:"nan",title:"Bad progress",state:"running",progress:NaN}])).toThrow("finite");
  });
});
