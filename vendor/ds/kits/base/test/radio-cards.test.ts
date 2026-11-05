import { describe, expect, it, vi } from "vitest";
import { flushSync } from "svelte";
import RadioGroup from "../src/RadioGroup.svelte";
import { render } from "./mount";

describe("RadioGroup rich choices",()=>{
  it("keeps native labels, shared names, descriptions and unavailable reasons",()=>{
    const changed=vi.fn();const root=render(RadioGroup,{legend:"Topology",name:"topology",variant:"cards",value:"single",help:"Choose one topology.",required:true,onvaluechange:changed,options:[
      {value:"single",label:"Single node",description:"For development."},
      {value:"replicated",label:"Replicated",description:"For failover."},
      {value:"multi",label:"Multi-region",description:"Enable another region first.",disabled:true},
    ]});
    const radios=root.querySelectorAll<HTMLInputElement>('input[type="radio"]');
    expect([...radios].map(radio=>radio.name)).toEqual(["topology","topology","topology"]);
    expect(radios[0]!.checked).toBe(true);expect(radios[2]!.disabled).toBe(true);
    const descriptions=radios[2]!.getAttribute("aria-describedby")!.split(" ").map(id=>document.getElementById(id)?.textContent);
    expect(descriptions).toEqual(["Choose one topology.","Enable another region first."]);
    radios[1]!.click();flushSync();expect(changed).toHaveBeenCalledExactlyOnceWith("replicated");expect(radios[0]!.checked).toBe(false);
    expect(radios[1]!.required).toBe(true);
  });
});
