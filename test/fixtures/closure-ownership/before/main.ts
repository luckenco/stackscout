export function tsOuter() {
  const callback = () => hiddenTs();
  visibleTs();
}

function hiddenTs() {}
function hiddenTsOther() {}
function visibleTs() {}
function visibleTsExtra() {}
