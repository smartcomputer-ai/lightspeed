import { describe, expect, it } from "vitest";
import { cronBuilderFromExpression, cronFromBuilder, type CronBuilderState } from "./cron-builder";

const base: CronBuilderState = {
  frequency: "daily",
  interval: 15,
  minute: 0,
  time: "09:30",
  weekday: 1,
  monthday: 1,
};

describe("visual cron builder", () => {
  it("generates the supported five-field schedules", () => {
    expect(cronFromBuilder({ ...base, frequency: "minutes", interval: 10 })).toBe("*/10 * * * *");
    expect(cronFromBuilder({ ...base, frequency: "hourly", minute: 12 })).toBe("12 * * * *");
    expect(cronFromBuilder({ ...base, frequency: "daily" })).toBe("30 9 * * *");
    expect(cronFromBuilder({ ...base, frequency: "weekdays" })).toBe("30 9 * * 1-5");
    expect(cronFromBuilder({ ...base, frequency: "weekly", weekday: 4 })).toBe("30 9 * * 4");
    expect(cronFromBuilder({ ...base, frequency: "monthly", monthday: 20 })).toBe("30 9 20 * *");
  });

  it("initializes from common existing expressions", () => {
    expect(cronBuilderFromExpression("*/20 * * * *")).toMatchObject({ frequency: "minutes", interval: 20 });
    expect(cronBuilderFromExpression("45 6 * * 1-5")).toMatchObject({ frequency: "weekdays", time: "06:45" });
    expect(cronBuilderFromExpression("0 18 * * 5")).toMatchObject({ frequency: "weekly", weekday: 5, time: "18:00" });
    expect(cronBuilderFromExpression("15 7 12 * *")).toMatchObject({ frequency: "monthly", monthday: 12, time: "07:15" });
  });
});
