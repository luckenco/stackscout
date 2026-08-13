export function tsOuter() {
  const callback = () => hiddenTsOther();
  visibleTs();
  visibleTsExtra();
}

function hiddenTs() {}
function hiddenTsOther() {}
function visibleTs() {}
function visibleTsExtra() {}
