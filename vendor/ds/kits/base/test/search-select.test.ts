import { describe, expect, it } from "vitest";
import { SearchSelect, searchSelectOptions } from "../src/index";
import { render } from "./mount";

const options = [
  { value: "gru", label: "São Paulo", description: "Brazil · South America", keywords: ["sa-east-1"] },
  { value: "fra", label: "Frankfurt", description: "Germany · Europe", disabled: true },
];

describe("searchable dropdown selection", () => {
  it("matches accents, descriptions, aliases and all query terms without mutating choices", () => {
    expect(searchSelectOptions(options, "sao paulo")).toEqual([options[0]]);
    expect(searchSelectOptions(options, "BRAZIL sao")).toEqual([options[0]]);
    expect(searchSelectOptions(options, "sa-east-1")).toEqual([options[0]]);
    expect(searchSelectOptions(options, "europe")).toEqual([options[1]]);
    expect(searchSelectOptions(options, "not present")).toEqual([]);
    expect(searchSelectOptions(options, "   ")).toBe(options);
  });

  it("submits a selected single value while its search popup is closed", () => {
    const root = render(SearchSelect, { label: "Region", options, name: "region", value: "gru", required: true });
    const form = document.createElement("form"); root.before(form); form.append(root);
    const select = root.querySelector("select")!;
    expect(new FormData(form).get("region")).toBe("gru");
    expect(select.checkValidity()).toBe(true);
    expect(document.querySelector("[data-search-select-surface]")).toBeNull();
    expect(root.querySelector("[data-search-select-trigger]")?.textContent).toContain("São Paulo");
    form.replaceWith(root);
  });

  it("submits multiple native entries and makes disabled selections inert", () => {
    const root = render(SearchSelect, { label: "Regions", options, name: "region", multiple: true, value: ["gru", "fra"], disabled: true });
    const form = document.createElement("form"); root.before(form); form.append(root);
    expect([...root.querySelectorAll("option:checked")].map(option => (option as HTMLOptionElement).value)).toEqual(["gru", "fra"]);
    expect(new FormData(form).getAll("region")).toEqual([]);
    expect(root.querySelectorAll("[data-search-select-chip]")).toHaveLength(2);
    expect([...root.querySelectorAll("button")].every(button => button.disabled)).toBe(true);
    form.replaceWith(root);
  });
});
